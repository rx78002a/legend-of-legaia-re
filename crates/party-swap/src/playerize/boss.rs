//! Boss swap: a player slot wears a *monster's own* battle model **and** its
//! own motions.
//!
//! The Delilas party swap ([`super::playerize_player_file`]) bakes the
//! sibling's geometry into the host's rest pose so the host's retail clips
//! still drive it; the cost is every clip being the host's, and the bake
//! deforming the mesh whenever a clip leaves the rest pose.
//!
//! This path keeps the monster's geometry **unbaked** (object-local, exactly
//! as authored) and replaces the host's clips instead:
//!
//! * every clip of the monster is read from its archive block (`+0x4C` entry
//!   table, stream at entry `+0x8C`);
//! * its part rows are re-addressed into the player's channel order
//!   (the anatomy permutation, [`PlayerRig::channel_for_canonical`]) - the
//!   *only* change to the pose data, so the pose model the monster was
//!   authored for is preserved bit for bit;
//! * the entry head (`root speed`, `end step`, hit-event frames, effect
//!   script, sound-cue track, `rate`) comes from the monster clip, so the
//!   hits land on the monster's own beats; the player-side fields that carry
//!   game rules (power run, gate, loop windows, solo flag, impact class)
//!   stay the host's.
//!
//! The pieces rewritten per player file: `record[0]` (inline reaction /
//! idle / walk / block clips and the art-bank heads), every equipment
//! section's swing records (slots `0xC..=0xF`), and - in `readef.DAT` - the
//! character's main and base `"ME"` archives.

use super::super::*;
use super::{
    COMPANION_PAL_BASE, decode_record, monster_pool_texels, rebuild_player_file_unbounded,
    relayout_to_band, rewrite_section_record,
};
use crate::winpose::{READEF_SLOT, encode_channel_delta, pack_part};
use legaia_actor_formats::me_archive;
use legaia_actor_formats::monster_archive::parse_animation_stream;

/// Bytes of a player action-entry head (stream follows at `+0xAC`).
pub const PLAYER_HEAD: usize = 0xAC;
/// Bytes of a monster action-entry head (stream follows at `+0x8C`).
pub const MONSTER_HEAD: usize = 0x8C;
/// `u32` words of the record[0] action table.
const ACTION_SLOTS: usize = 12;
/// Offset of the art-bank word and of its paired word inside record[0].
const BANK_WORD: usize = 0x58;
const BANK_PAIR_WORD: usize = 0x5C;
/// Stride of one art-bank record and the offset of its embedded entry.
const ART_STRIDE: usize = 0xD0;
const ART_ENTRY: usize = 0x24;
/// `0xFF` in entry `+0x84` marks the base ("victory") archive records.
const BASE_LOOP: u8 = 0xFF;
/// Persistent-buffer ceiling the battle member init allocates for
/// `record[0]` plus the sequentially decoded equipment sections.
pub const MEMBER_ALLOC: usize = 0x19000;

/// One clip of the boss, in the boss's own part order.
#[derive(Debug, Clone)]
pub struct BossClip {
    /// Index in the monster's `+0x4C` entry table.
    pub entry: usize,
    /// Action tag (entry `+0x00`).
    pub tag: u8,
    /// The monster entry head, `+0x00..+0x8C`.
    pub head: Vec<u8>,
    /// `frames[f][part]`, boss part order.
    pub frames: Vec<Vec<PartPose>>,
}

impl BossClip {
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// The clip at `1/k` of the frames and `1/k` of the rate (the cursor
    /// advances in proportion to the rate, so the duration holds when the rate
    /// can halve; a rate of 1 cannot, and that clip plays `k` times faster):
    /// every second pose dropped. Every frame-indexed head field moves with it - hit
    /// events, effect-script frames, sound cues and the loop window. For a
    /// boss whose long clips overflow the player file's record[0] budget.
    pub fn decimated(&self, k: usize) -> BossClip {
        let k = k.max(1);
        let frames: Vec<_> = self.frames.iter().step_by(k).cloned().collect();
        let mut head = self.head.clone();
        let scale = |f: u8| ((f as usize / k).min(255)) as u8;
        for b in &mut head[0x10..0x14] {
            if *b != 0 {
                *b = scale(*b).max(1);
            }
        }
        for r in 0..8 {
            let o = 0x14 + r * 8;
            if head[o + 1] != 0 || head[o + 2..o + 8].iter().any(|&b| b != 0) {
                head[o] = scale(head[o]);
            }
        }
        for o in (0x54..0x74).step_by(4) {
            if head[o + 2] != 0 || head[o + 3] != 0 {
                let f = u16::from_le_bytes([head[o], head[o + 1]]) as usize / k;
                head[o..o + 2].copy_from_slice(&(f as u16).to_le_bytes());
            }
        }
        if head[0x84] != 0 {
            head[0x85] = scale(head[0x85]);
            head[0x86] = scale(head[0x86]).max(head[0x85].saturating_add(1));
        }
        // The cursor advances in proportion to the rate: with `1/k` of the
        // poses the same duration needs `1/k` of the rate. A rate of 1 cannot
        // go lower, so such a clip plays `k` times faster.
        head[0x78] = (head[0x78].max(1) as usize / k).max(1) as u8;
        BossClip {
            frames,
            head,
            ..self.clone()
        }
    }

    /// Non-zero hit-event frames (entry `+0x10..+0x13`).
    pub fn events(&self) -> Vec<u8> {
        self.head[0x10..0x14]
            .iter()
            .copied()
            .take_while(|&b| b != 0)
            .collect()
    }

    pub fn rate(&self) -> u8 {
        self.head[0x78]
    }
}

/// Every populated clip of monster `id`, in entry-table order.
pub fn boss_clips(archive: &[u8], id: u16) -> Result<Vec<BossClip>> {
    let block = monster_archive::decode_block(archive, id)?
        .ok_or_else(|| anyhow::anyhow!("monster id {id}: empty slot"))?;
    let n = *block.get(0x4A).context("monster record too short")? as usize;
    let mut out = Vec::new();
    for i in 0..n {
        let eo = legaia_bytes::u32_le(&block, 0x4C + i * 4)
            .ok_or_else(|| anyhow::anyhow!("entry table truncated at {i}"))?
            as usize;
        let Some(head) = block.get(eo..eo + MONSTER_HEAD) else {
            continue;
        };
        let tag = head[0];
        let Some(anim) =
            parse_animation_stream(&block, tag, head[0x78], 0, eo + MONSTER_HEAD, Vec::new())
        else {
            continue;
        };
        out.push(BossClip {
            entry: i,
            tag,
            head: head.to_vec(),
            frames: anim.frames,
        });
    }
    if out.is_empty() {
        bail!("monster id {id} carries no clips");
    }
    Ok(out)
}

/// What a boss clip does for the player slot it fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Idle / walk / reactions: the host's power run and tag stay.
    Reaction,
    /// Swings and arts: attach key cleared, power run filled out to the
    /// clip's hit count.
    Attack,
}

fn scale_pose(p: &PartPose, s: f32) -> PartPose {
    if (s - 1.0).abs() < 1e-6 {
        return *p;
    }
    let sc = |v: i16| ((v as f32 * s).round() as i32).clamp(-2048, 2047) as i16;
    PartPose {
        tx: sc(p.tx),
        ty: sc(p.ty),
        tz: sc(p.tz),
        ..*p
    }
}

/// `[parts][frames][rows]` for `frames` (boss part order) with every row
/// re-addressed into the player's channel order. Channels the boss has no
/// part for (Noa's hair, equipment extras past the skeleton) are zero poses.
/// Padded to a word multiple, the unit the retail swing splice copies.
pub fn player_stream(
    frames: &[Vec<PartPose>],
    rig: &PlayerRig,
    parts: usize,
    scale: f32,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + frames.len() * parts * 9);
    out.push(parts as u8);
    out.push(frames.len() as u8);
    let mut src_of = vec![None; parts];
    for (c, &ch) in rig.channel_for_canonical.iter().enumerate() {
        if (ch as usize) < parts {
            src_of[ch as usize] = Some(c);
        }
    }
    for frame in frames {
        for slot in src_of.iter().take(parts) {
            let pose = slot
                .and_then(|c| frame.get(c))
                .map(|p| scale_pose(p, scale))
                .unwrap_or_default();
            out.extend_from_slice(&pack_part(&pose));
        }
    }
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
    out
}

/// Overwrite the fields of a player entry head that belong to the *clip*
/// with the boss clip's, leaving the ones that carry rules alone.
fn apply_clip_head(entry: &mut [u8], clip: &BossClip, nframes: usize, kind: Kind) {
    let h = &clip.head;
    entry[0x0C..0x10].copy_from_slice(&h[0x0C..0x10]);
    entry[0x10..0x14].copy_from_slice(&h[0x10..0x14]);
    // An event frame at or past the clip end would never fire.
    for b in &mut entry[0x10..0x14] {
        if *b as usize >= nframes {
            *b = nframes.saturating_sub(1) as u8;
        }
    }
    entry[0x14..0x54].copy_from_slice(&h[0x14..0x54]);
    entry[0x54..0x74].copy_from_slice(&strip_voice_cues(&h[0x54..0x74]));
    entry[0x78] = h[0x78].max(1);
    // Face tracks and equipment-variant windows belong to the host's
    // face and weapon, not to the boss.
    entry[0x8C..0xAC].fill(0);
    if kind == Kind::Attack {
        entry[0x77] = 0;
        let hits = entry[0x10..0x14].iter().filter(|&&b| b != 0).count().min(4);
        let last = entry[0..4].iter().rev().copied().find(|&b| b != 0);
        if let Some(last) = last {
            for b in entry[..hits].iter_mut().filter(|b| **b == 0) {
                *b = last;
            }
        }
    }
}

/// Sound cues are `[u16 frame][u16 id]` pairs. For a party actor an id
/// `>= 0x100` selects an XA voice line by position in the *party's* voice
/// table, so a boss's own voice ids would play whatever party line sits at
/// that slot - long, unrelated shouts. Drop those pairs and close the gap
/// (a zero pair ends the list); the effect ids below `0x100` keep playing.
fn strip_voice_cues(cues: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(cues.len());
    for pair in cues.chunks_exact(4) {
        let id = u16::from_le_bytes([pair[2], pair[3]]);
        if id >= 0x100 {
            continue;
        }
        if pair == [0, 0, 0, 0] {
            break;
        }
        out.extend_from_slice(pair);
    }
    out.resize(cues.len(), 0);
    out
}

fn set_loop(entry: &mut [u8], count: u8, start: usize, end: usize) {
    entry[0x84] = count;
    entry[0x85] = start.min(255) as u8;
    entry[0x86] = end.min(255) as u8;
}

/// Keep a host loop window inside a clip of `n` frames.
fn clamp_loop(entry: &mut [u8], n: usize) {
    if entry[0x84] == 0 {
        return;
    }
    let (s, e) = (entry[0x85] as usize, entry[0x86] as usize);
    let e = e.min(n);
    let s = s.min(e.saturating_sub(1));
    entry[0x85] = s as u8;
    entry[0x86] = e as u8;
}

// ---------------------------------------------------------------------------
// Clip selection

/// Which boss clip fills each player-side role.
#[derive(Debug, Clone)]
pub struct ClipMap {
    /// record[0] slot -> boss clip (by position in the clip list).
    pub slots: BTreeMap<usize, usize>,
    /// Slot 9 (defeat) holds its last frames.
    pub defeat_hold: bool,
    /// Direction swings `0xC..=0xF`.
    pub swings: [usize; 4],
    /// Hit-ful clips for arts, longest multi-hit first.
    pub multi_hit: usize,
    pub single_hit: Vec<usize>,
    /// The boss's walk (used for hit-less art records).
    pub walk: usize,
    /// Victory source and the idle it settles into.
    pub victory: usize,
    pub idle: usize,
    /// Calm clip the Spirit command (art record 0) plays; `None`
    /// keeps the host's stream there.
    pub spirit: Option<usize>,
}

/// Tag the synthesized melee clips carry (a single-hit move).
const SYNTH_MELEE_TAG: u8 = 0x0E;

/// For a boss with no melee of its own (Cort's first form only casts): add
/// single-hit clips cloned from its two shortest cast clips, one hit event on
/// the pose the cast holds (or two thirds in), no effect script, sound cue or
/// loop window. Returns how many were added; none when the boss attacks.
pub fn add_cast_melee(clips: &mut Vec<BossClip>) -> usize {
    let attack = |c: &BossClip| (0x0C..=0x1F).contains(&c.tag) && !c.events().is_empty();
    if clips.iter().any(attack) {
        return 0;
    }
    let mut casts: Vec<BossClip> = clips.iter().filter(|c| c.tag == 0x23).cloned().collect();
    casts.sort_by_key(BossClip::len);
    let mut added = 0;
    for mut c in casts.into_iter().take(2) {
        let n = c.len();
        let ev = if c.head[0x84] != 0 {
            c.head[0x85] as usize
        } else {
            n * 2 / 3
        };
        c.head[0x10..0x14].copy_from_slice(&[ev.clamp(1, n.saturating_sub(1)) as u8, 0, 0, 0]);
        c.head[0x14..0x74].fill(0);
        c.head[0x84..0x87].fill(0);
        c.tag = SYNTH_MELEE_TAG;
        c.head[0] = SYNTH_MELEE_TAG;
        c.entry += 0x100;
        clips.push(c);
        added += 1;
    }
    added
}

/// Rate-1 clips up to this many frames are never decimated (halving the
/// poses would double their speed).
const RATE1_KEEP: usize = 24;

fn find_tag(clips: &[BossClip], tag: u8) -> Option<usize> {
    clips.iter().position(|c| c.tag == tag)
}

/// The clip map for Songi-shaped bosses (idle/walk/reactions by tag, `0x0E`
/// single-hit moves, a `0x0D` multi-hit move, `0x22` taunt).
pub fn default_clip_map(clips: &[BossClip]) -> Result<ClipMap> {
    let need = |t: u8| find_tag(clips, t).with_context(|| format!("boss has no tag {t:#x} clip"));
    let idle = need(0)?;
    let walk = need(1)?;
    let mut slots = BTreeMap::new();
    for (slot, tag) in [
        (0usize, 0u8),
        (1, 1),
        (2, 2),
        (3, 3),
        (4, 4),
        (5, 5),
        (0xB, 0xB),
    ] {
        // A boss that never blocks plays its idle for the host's Block.
        let ci = match find_tag(clips, tag) {
            Some(ci) => ci,
            None if tag == 0xB => idle,
            None => need(tag)?,
        };
        slots.insert(slot, ci);
    }
    slots.insert(7, idle);
    slots.insert(8, idle);
    slots.insert(9, need(4)?);
    // Melee moves, best fit first: the tag-0x0E single-hit moves, then any
    // other single-hit attack, then the multi-hit attacks; cycled to four.
    let attack = |c: &BossClip| (0x0C..=0x1F).contains(&c.tag) && !c.events().is_empty();
    let mut singles: Vec<usize> = Vec::new();
    for pass in 0..3 {
        for (i, c) in clips.iter().enumerate() {
            let fits = match pass {
                0 => c.tag == 0x0E && c.events().len() == 1,
                1 => attack(c) && c.events().len() == 1,
                _ => attack(c) && c.events().len() >= 2,
            };
            if fits && !singles.contains(&i) {
                singles.push(i);
            }
        }
    }
    if singles.is_empty() {
        bail!("boss has no attack clip with a hit event");
    }
    let found = singles.len();
    while singles.len() < 4 {
        singles.push(singles[singles.len() % found]);
    }
    let multi = clips
        .iter()
        .enumerate()
        .filter(|(_, c)| c.events().len() >= 2 && (0x0C..=0x1F).contains(&c.tag))
        .max_by_key(|(_, c)| c.len())
        .or_else(|| {
            clips
                .iter()
                .enumerate()
                .filter(|(_, c)| attack(c))
                .max_by_key(|(_, c)| c.len())
        })
        .map(|(i, _)| i)
        .context("boss has no multi-hit move")?;
    let victory = find_tag(clips, 0x22).unwrap_or(idle);
    Ok(ClipMap {
        slots,
        defeat_hold: true,
        swings: [singles[0], singles[1], singles[2], singles[3]],
        multi_hit: multi,
        single_hit: singles,
        walk,
        victory,
        idle,
        spirit: None,
    })
}

/// record[0] action row that holds the High swing's melee clip when the
/// fireball owns swing `0xF`. Retail leaves the row a header-only placeholder
/// (the cast-module route stages it on a player caster the same way): a queue
/// byte `0x0A` commits it through the party arm of the anim commit.
pub const UP_MELEE_SLOT: usize = 0x0A;

/// A record[0] row authored as a **caster stage pose** for a cast module.
///
/// A capture-class cast module drives the caster's body by writing a raw
/// action index into `actor+0x1DA`; on a party caster the index resolves
/// through the 12-word table at the head of record[0]. This is the
/// proven-safe entry shape (`party_swap::cast_stage`): a zero head except the
/// row id, the attach key `0x0A` at `+0x77`, the rate, and a hold window that
/// parks the pose strictly inside the stream, so the clip's natural end never
/// re-commits it from frame 0 while the module is still dwelling. No effect
/// script, no hit event, no sound cue ride along; the module owns those.
pub fn stage_entry(
    row: usize,
    clip: &BossClip,
    rig: &PlayerRig,
    parts: usize,
    scale: f32,
) -> Vec<u8> {
    let n = clip.len();
    let mut e = vec![0u8; PLAYER_HEAD];
    e[0] = row as u8;
    e[0x77] = UP_MELEE_SLOT as u8;
    e[0x78] = clip.head[0x78].max(1);
    // The clip's own hold window: retail casts park on the pose the move
    // is about to release (Crush's fist on the ground, not the recovery
    // after it). A clip without one parks near its end.
    let (cnt, hs, he) = (clip.head[0x84], clip.head[0x85], clip.head[0x86]);
    if cnt != 0 && (hs as usize) < n {
        let hs = hs as usize;
        let he = (he as usize).clamp(hs, n.saturating_sub(1));
        set_loop(&mut e, cnt, hs, he);
    } else {
        let park = n.saturating_sub(2);
        set_loop(&mut e, 0xFF, park, park);
    }
    e.extend_from_slice(&player_stream(&clip.frames, rig, parts, scale));
    e
}

/// A stage pose as the plan wants it embedded, decimated by `k` (1 = whole).
fn stage_clip(k: usize, clip: &BossClip) -> BossClip {
    if k > 1 {
        clip.decimated(k)
    } else {
        clip.clone()
    }
}

/// Frames between consecutive hits of a multi-hit art playing the fireball.
const FIREBALL_HIT_GAP: usize = 2;
/// `[count, start, end]` hold window the boss's own cast uses to charge.
const FIREBALL_LOOP: (u8, usize, usize) = (10, 10, 11);
/// Power run of the fireball art: Vahn's own Hyper-art power bytes (Burning
/// Flare), four hits. The projectile's move-power record carries no damage of
/// its own (see `boss-swap`), so the art's hits are what damage and kill.
const FIREBALL_POWER: [u8; 4] = [0x1D, 0x19, 0x1F, 0x1A];
/// Inline name the fireball art is given (HUD banner).
const FIREBALL_NAME: &str = "Hyper Wave";
/// Main ME entry the fireball art streams from (the first spare stub).
const FIREBALL_ENTRY: usize = 6;

/// Append the boss's cast clip (tag `0x23`) to `clips` as a player art clip
/// and return its index. The clip keeps its move-power terminator record: the
/// terminator installs the move-power record the actor's queued move id maps
/// to, and that record spawns the projectile. One hit event takes the
/// terminator's frame so the art also lands a hit of its own, and the clip
/// runs at twice the monster's cast rate.
pub fn add_fireball_clip(clips: &mut Vec<BossClip>) -> Result<usize> {
    let cast = find_tag(clips, 0x23).context("boss has no tag 0x23 cast clip")?;
    let mut play = clips[cast].clone();
    let release = (0..8)
        .map(|r| 0x14 + r * 8)
        .find(|&o| play.head[o] != 0 && play.head[o + 1] & 0x7F == 0x7F)
        .map(|o| play.head[o])
        .context("boss cast clip has no move-power terminator")?;
    play.head[0x10..0x14].copy_from_slice(&[release, 0, 0, 0]);
    play.head[0x78] = play.head[0x78].max(2);
    clips.push(play);
    Ok(clips.len() - 1)
}

/// Frames the Spirit clip runs (the art stream is addressed up to row 58).
const SPIRIT_FRAMES: usize = 59;
/// Main ME entry the multi-hit move moves to when entry 0 holds the Spirit clip.
const MULTI_ENTRY: usize = 7;

/// Append two calm clips made of the boss idle and return `(pose, spirit)`:
/// `pose` is the idle the host's slot 9 plays for an item, a spell or a
/// defeat, and `spirit` cycles the idle out to the Spirit command's length.
/// Both carry the idle's head, so no hit event, effect or sound rides along.
pub fn add_calm_clips(clips: &mut Vec<BossClip>) -> Result<(usize, usize)> {
    let idle = find_tag(clips, 0).context("boss has no idle clip")?;
    let src = clips[idle].clone();
    let mut head = src.head.clone();
    head[0x10..0x14].fill(0);
    head[0x14..0x74].fill(0);
    head[0x84..0x87].fill(0);
    let cycled: Vec<Vec<PartPose>> = (0..SPIRIT_FRAMES)
        .map(|i| src.frames[i % src.frames.len()].clone())
        .collect();
    clips.push(BossClip {
        head: head.clone(),
        frames: src.frames.clone(),
        ..src.clone()
    });
    clips.push(BossClip {
        head,
        frames: cycled,
        ..src
    });
    Ok((clips.len() - 2, clips.len() - 1))
}

/// Fireball timing for an art record entry: `hits` beats from the release
/// frame, and the boss's charge hold.
fn fireball_timing(entry: &mut [u8], clip: &BossClip, hits: usize, n: usize) {
    // The hits land after the projectile is away.
    let first = clip.events().first().copied().unwrap_or(1) as usize + FIREBALL_HIT_GAP;
    entry[0x10..0x14].fill(0);
    for i in 0..hits.min(4) {
        entry[0x10 + i] = (first + i * FIREBALL_HIT_GAP).min(n.saturating_sub(1)) as u8;
    }
    set_loop(entry, FIREBALL_LOOP.0, FIREBALL_LOOP.1, FIREBALL_LOOP.2);
}

// ---------------------------------------------------------------------------
// record[0]

/// The file header words and decoded record[0] of a player file.
#[derive(Debug, Clone)]
pub struct Record0 {
    pub desc_off: usize,
    pub clut_a: usize,
    pub clut_b: usize,
    pub budget: usize,
    pub decoded: Vec<u8>,
}

impl Record0 {
    pub fn parse(file: &[u8]) -> Result<Self> {
        let w = |o: usize| -> Result<usize> {
            Ok(legaia_bytes::u32_le(file, o).context("short player-file header")? as usize)
        };
        let (desc_off, clut_a, clut_b, budget) = (w(0)?, w(4)?, w(8)?, w(0xC)?);
        let decoded = legaia_lzs::decompress(file.get(0x10..).context("short file")?, budget)?;
        Ok(Self {
            desc_off,
            clut_a,
            clut_b,
            budget,
            decoded,
        })
    }

    fn word(&self, o: usize) -> usize {
        legaia_bytes::u32_le(&self.decoded, o).unwrap_or(0) as usize
    }

    /// Byte offset of the action entry of `slot`, if populated.
    pub fn entry_offset(&self, slot: usize) -> Option<usize> {
        let o = self.word(slot * 4);
        (o != 0 && o + PLAYER_HEAD + 2 <= self.decoded.len()).then_some(o)
    }

    /// `(parts, frames)` of `slot`'s stream.
    pub fn stream_shape(&self, slot: usize) -> Option<(usize, usize)> {
        let o = self.entry_offset(slot)?;
        Some((
            self.decoded[o + PLAYER_HEAD] as usize,
            self.decoded[o + PLAYER_HEAD + 1] as usize,
        ))
    }
}

/// Per art-bank record: what it plays.
#[derive(Debug, Clone)]
pub struct ArtUse {
    pub index: usize,
    pub base: bool,
    pub stream_source: usize,
    /// Stream index the record is repointed to (shared between arts so the
    /// archive stores each boss clip once).
    pub new_source: usize,
    pub hits: usize,
    /// Inline art name (record `+0x10`).
    pub name: String,
    /// This record plays the fireball clip.
    pub fireball: bool,
}

/// What a main-archive entry holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainEntry {
    /// A boss clip.
    Clip(usize),
    /// One frame of the boss idle - a valid stream nothing plays.
    Stub,
}

/// A full plan: which boss clip every archive entry and bank record uses.
#[derive(Debug, Clone)]
pub struct BossPlan {
    pub map: ClipMap,
    pub arts: Vec<ArtUse>,
    /// Main archive entry contents.
    pub main_entries: Vec<MainEntry>,
    /// Base archive entry count (all carry the victory composite).
    pub base_entries: usize,
    /// The fireball clip, when an art plays it.
    pub fireball: Option<usize>,
    /// The fireball clip, when direction swing `0xF` plays it (the charge
    /// hold is applied to that swing record).
    pub fireball_swing: Option<usize>,
    /// Clip the host's record[0] action row [`UP_MELEE_SLOT`] plays as a
    /// melee swing (the High swing the fireball took over), when set.
    pub up_melee: Option<usize>,
    /// `(record[0] row, clip)`: placeholder rows authored as caster stage
    /// poses for the cast-module route (see [`stage_entry`]).
    pub stage_rows: Vec<(usize, usize)>,
    /// Clips authored as extra stage entries after the slot entries, outside
    /// the 12-word table: a cast stub repoints a table word at one of them at
    /// run time. Their record[0] offsets come back in
    /// [`Bossized::pool_offsets`].
    pub stage_pool: Vec<usize>,
    /// Clips longer than this many frames are decimated (half the poses,
    /// half the rate) where record[0] embeds them. Swings and arts keep every
    /// pose: only record[0] has the tight budget.
    pub decimate_over: Option<usize>,
    /// Factor the stage rows are decimated by (they are parked poses, so
    /// fewer poses cost little); `1` keeps every pose.
    pub decimate_stage: usize,
    /// The same for the pooled stage poses.
    pub decimate_pool: usize,
    /// Rows 7 and 8 (idle placeholders) are decimated by two.
    pub decimate_filler: bool,
}

fn power_hits(rec: &[u8]) -> usize {
    rec[ART_ENTRY..ART_ENTRY + 4]
        .iter()
        .filter(|&&b| b != 0)
        .count()
}

/// Decide the clip every art record / archive entry plays.
pub fn plan_boss(
    rec0: &Record0,
    clips: &[BossClip],
    main_entries: usize,
    base_entries: usize,
) -> Result<BossPlan> {
    plan_boss_with(
        rec0,
        default_clip_map(clips)?,
        main_entries,
        base_entries,
        None,
    )
}

/// [`plan_boss`] over an explicit clip map (see [`fireball_map`]).
/// `fireball` is `(clip index, art name)`: the art record of that inline name
/// plays the clip instead of a melee move.
pub fn plan_boss_with(
    rec0: &Record0,
    map: ClipMap,
    main_entries: usize,
    base_entries: usize,
    fireball: Option<(usize, &str)>,
) -> Result<BossPlan> {
    let bank_off = rec0.word(BANK_WORD);
    let count = rec0.word(bank_off);
    if count == 0 || count > 0x40 {
        bail!("implausible art bank count {count}");
    }
    let mut arts: Vec<ArtUse> = Vec::new();
    for k in 0..count {
        let b = bank_off + 4 + k * ART_STRIDE;
        let rec = rec0
            .decoded
            .get(b..b + ART_STRIDE)
            .context("art bank record past record[0]")?;
        arts.push(ArtUse {
            index: k,
            base: rec[ART_ENTRY + 0x84] == BASE_LOOP,
            stream_source: rec[0x0A] as usize,
            new_source: rec[0x0A] as usize,
            hits: power_hits(rec),
            name: String::from_utf8_lossy(
                rec[0x10..ART_ENTRY]
                    .split(|&b| b == 0)
                    .next()
                    .unwrap_or(&[]),
            )
            .into_owned(),
            fireball: false,
        });
    }
    if main_entries < 6 {
        bail!("main ME archive has only {main_entries} entries");
    }
    // Entry 0 doubles as the Spirit-charge hold stream (addressed up to row
    // 58), so it carries the longest clip; entries 1..=4 the single-hit
    // moves, 5 the walk, the rest are stubs. Art records are repointed
    // onto these so each boss clip is stored once.
    let mut main = vec![MainEntry::Stub; main_entries];
    main[0] = MainEntry::Clip(map.multi_hit);
    for (i, &c) in map.single_hit.iter().take(4).enumerate() {
        main[1 + i] = MainEntry::Clip(c);
    }
    main[5] = MainEntry::Clip(map.walk);
    let spirit = map.spirit;
    if let Some(c) = spirit {
        if main_entries <= MULTI_ENTRY {
            bail!("main ME archive has no spare entry for the multi-hit move");
        }
        main[0] = MainEntry::Clip(c);
        main[MULTI_ENTRY] = MainEntry::Clip(map.multi_hit);
    }
    let mut single_i = 0usize;
    for a in arts.iter_mut().filter(|a| !a.base) {
        a.new_source = if a.stream_source == 0 {
            if spirit.is_some() && a.index != 0 {
                MULTI_ENTRY
            } else {
                0
            }
        } else if a.hits == 0 {
            5
        } else if a.hits == 1 {
            single_i += 1;
            1 + (single_i - 1) % 4
        } else {
            0
        };
    }
    let mut fireball_clip = None;
    if let Some((clip, name)) = fireball {
        if main_entries <= FIREBALL_ENTRY {
            bail!("main ME archive has no spare entry for the fireball");
        }
        let art = arts
            .iter_mut()
            .find(|a| !a.base && a.name.eq_ignore_ascii_case(name))
            .with_context(|| format!("no art record named {name:?}"))?;
        art.fireball = true;
        art.new_source = FIREBALL_ENTRY;
        main[FIREBALL_ENTRY] = MainEntry::Clip(clip);
        fireball_clip = Some(clip);
    }
    Ok(BossPlan {
        map,
        arts,
        main_entries: main,
        base_entries,
        fireball: fireball_clip,
        fireball_swing: None,
        up_melee: None,
        stage_rows: Vec::new(),
        stage_pool: Vec::new(),
        decimate_over: None,
        decimate_stage: 1,
        decimate_pool: 1,
        decimate_filler: false,
    })
}

/// Build one inline record[0] entry (head + stream, word padded).
fn inline_entry(
    old_head: &[u8],
    clip: &BossClip,
    rig: &PlayerRig,
    parts: usize,
    scale: f32,
    slot: usize,
    defeat_hold: bool,
) -> Vec<u8> {
    let n = clip.len();
    let mut e = old_head[..PLAYER_HEAD].to_vec();
    apply_clip_head(&mut e, clip, n, Kind::Reaction);
    e[0] = slot as u8;
    if slot == 9 && defeat_hold {
        set_loop(&mut e, 10, n.saturating_sub(2), n.saturating_sub(1));
    } else {
        // Idle/walk/reactions never loop through the hold counter.
        e[0x84..0x87].fill(0);
    }
    e.extend_from_slice(&player_stream(&clip.frames, rig, parts, scale));
    e
}

/// Rewrite record[0]: inline entries, art-bank heads. Returns the new
/// decoded block plus the new header words `(clut_a, clut_b, budget)`.
pub fn rebuild_record0(
    rec0: &Record0,
    clips: &[BossClip],
    plan: &BossPlan,
    rig: &PlayerRig,
    scale: f32,
    up_melee: Option<&[u8]>,
) -> Result<(Vec<u8>, [usize; 3], Vec<usize>)> {
    let d = &rec0.decoded;
    let bank_off = rec0.word(BANK_WORD);
    let bank_end = rec0.word(BANK_PAIR_WORD); // == clut_a - 4 (vestigial pair)
    let count = rec0.word(bank_off);
    let old_bank_end = bank_off + 4 + count * ART_STRIDE;
    if old_bank_end > rec0.clut_a {
        bail!("art bank runs into texture block A");
    }
    let _ = bank_end;

    // Entry spans: [off_i, off_{i+1}), the last ending at the bank.
    let offs: Vec<usize> = (0..ACTION_SLOTS).map(|i| rec0.word(i * 4)).collect();
    let mut out = d[..0x60].to_vec();
    let mut new_offs = [0usize; ACTION_SLOTS];
    for slot in 0..ACTION_SLOTS {
        let o = offs[slot];
        if o == 0 {
            continue;
        }
        let end = offs
            .iter()
            .copied()
            .filter(|&x| x > o)
            .min()
            .unwrap_or(bank_off);
        new_offs[slot] = out.len();
        let populated = rec0.stream_shape(slot).is_some_and(|(p, f)| p > 0 && f > 0);
        let stage = plan.stage_rows.iter().find(|(r, _)| *r == slot);
        match plan.map.slots.get(&slot) {
            _ if slot == UP_MELEE_SLOT && up_melee.is_some() => {
                out.extend_from_slice(up_melee.unwrap());
            }
            _ if stage.is_some() => {
                let parts = rec0.stream_shape(0).map(|s| s.0).unwrap_or(0);
                let (_, ci) = *stage.unwrap();
                let clip = stage_clip(plan.decimate_stage, &clips[ci]);
                out.extend_from_slice(&stage_entry(slot, &clip, rig, parts, scale));
            }
            Some(&ci) if populated => {
                let (parts, _) = rec0.stream_shape(slot).unwrap();
                let clip = match plan.decimate_over {
                    // A rate-1 clip cannot halve its rate, so decimating it
                    // would play it twice as fast: only the long ones go.
                    Some(cap)
                        if clips[ci].len() > cap
                            && (clips[ci].rate() >= 2 || clips[ci].len() > RATE1_KEEP) =>
                    {
                        clips[ci].decimated(2)
                    }
                    // Rows 7 and 8 only hold the idle as a placeholder for
                    // retail's chain rows: half its poses are enough.
                    _ if plan.decimate_filler && (slot == 7 || slot == 8) => clips[ci].decimated(2),
                    _ => clips[ci].clone(),
                };
                let e = inline_entry(
                    &d[o..o + PLAYER_HEAD],
                    &clip,
                    rig,
                    parts,
                    scale,
                    slot,
                    plan.map.defeat_hold,
                );
                out.extend_from_slice(&e);
            }
            _ => out.extend_from_slice(&d[o..end]),
        }
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
    }
    // Pool entries: stage poses nothing in the table points at.
    let mut pool_offsets = Vec::new();
    for &ci in &plan.stage_pool {
        pool_offsets.push(out.len());
        let parts = rec0.stream_shape(0).map(|s| s.0).unwrap_or(0);
        let clip = stage_clip(plan.decimate_pool, &clips[ci]);
        out.extend_from_slice(&stage_entry(UP_MELEE_SLOT, &clip, rig, parts, scale));
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
    }
    // Art bank: heads only (streams live in readef.DAT).
    let new_bank = out.len();
    out.extend_from_slice(&d[bank_off..old_bank_end]);
    for a in &plan.arts {
        let b = new_bank + 4 + a.index * ART_STRIDE;
        let (clip_idx, n) = art_clip(plan, clips, a);
        let clip = &clips[clip_idx];
        if !a.base {
            out[b + 0x0A] = a.new_source as u8;
        }
        if a.fireball {
            let name = FIREBALL_NAME.as_bytes();
            out[b + 0x10..b + ART_ENTRY].fill(0);
            out[b + 0x10..b + 0x10 + name.len()].copy_from_slice(name);
        }
        let entry = &mut out[b + ART_ENTRY..b + ART_STRIDE];
        if a.base {
            let (cnt, s, e) = victory_window(plan, clips);
            apply_clip_head(entry, &clips[plan.map.victory], n, Kind::Reaction);
            set_loop(entry, cnt, s, e);
        } else {
            apply_clip_head(entry, clip, n, Kind::Attack);
            clamp_loop(entry, n);
            if a.fireball {
                entry[..4].copy_from_slice(&FIREBALL_POWER);
                fireball_timing(entry, clip, FIREBALL_POWER.len(), n);
            }
        }
    }
    let pad = &d[old_bank_end..rec0.clut_a];
    out.extend_from_slice(pad);
    let new_clut_a = out.len();
    let delta = new_clut_a as i64 - rec0.clut_a as i64;
    out.extend_from_slice(&d[rec0.clut_a..]);
    for (slot, &o) in new_offs.iter().enumerate() {
        out[slot * 4..slot * 4 + 4].copy_from_slice(&(o as u32).to_le_bytes());
    }
    out[BANK_WORD..BANK_WORD + 4].copy_from_slice(&(new_bank as u32).to_le_bytes());
    out[BANK_PAIR_WORD..BANK_PAIR_WORD + 4]
        .copy_from_slice(&((new_clut_a as u32).saturating_sub(4)).to_le_bytes());
    let new_clut_b = (rec0.clut_b as i64 + delta) as usize;
    let budget = out.len();
    Ok((out, [new_clut_a, new_clut_b, budget], pool_offsets))
}

/// Clip + frame count an art record plays.
fn art_clip(plan: &BossPlan, clips: &[BossClip], a: &ArtUse) -> (usize, usize) {
    if a.base {
        let n = victory_frames(plan, clips).len();
        return (plan.map.victory, n);
    }
    match plan.main_entries.get(a.new_source) {
        Some(MainEntry::Clip(ci)) => (*ci, clips[*ci].len()),
        _ => (plan.map.walk, clips[plan.map.walk].len()),
    }
}

/// The victory composite (taunt, then the idle it settles into) and the loop
/// window over the idle tail.
fn victory_frames(plan: &BossPlan, clips: &[BossClip]) -> Vec<Vec<PartPose>> {
    let mut f = clips[plan.map.victory].frames.clone();
    if plan.map.victory != plan.map.idle {
        f.extend(clips[plan.map.idle].frames.iter().cloned());
    }
    f
}

fn victory_window(plan: &BossPlan, clips: &[BossClip]) -> (u8, usize, usize) {
    let total = victory_frames(plan, clips).len();
    let lead = if plan.map.victory != plan.map.idle {
        clips[plan.map.victory].len()
    } else {
        0
    };
    (
        BASE_LOOP,
        lead,
        total.saturating_sub(1).max(lead + 1).min(total),
    )
}

// ---------------------------------------------------------------------------
// readef.DAT archives

/// The rebuilt `READEF_SLOT` bytes of the character's main and base ME
/// archives.
pub struct RebuiltArchives {
    pub main: Vec<u8>,
    pub base: Vec<u8>,
    pub notes: Vec<String>,
}

fn me_slot(bodies: &[Vec<u8>]) -> Result<Vec<u8>> {
    let n = bodies.len();
    let used = 3 + 2 * n + bodies.iter().map(Vec::len).sum::<usize>();
    if used > READEF_SLOT {
        bail!("rebuilt ME archive needs {used} bytes, the slot holds {READEF_SLOT}");
    }
    let mut out = Vec::with_capacity(READEF_SLOT);
    out.extend_from_slice(&me_archive::MAGIC);
    out.push(n as u8);
    for b in bodies {
        if b.len() > 0x7FFF {
            bail!("ME body of {} bytes exceeds the 15-bit size word", b.len());
        }
        out.extend_from_slice(&((b.len() as u16) | 0x8000).to_le_bytes());
    }
    for b in bodies {
        out.extend_from_slice(b);
    }
    out.resize(READEF_SLOT, 0);
    Ok(out)
}

fn encode_body(stream: &[u8]) -> Result<Vec<u8>> {
    let enc = encode_channel_delta(stream)?;
    let back = me_archive::decode_channel_delta(&enc)?;
    let want = 2 + stream[0] as usize * stream[1] as usize * 9;
    if back.get(..want) != stream.get(..want) {
        bail!("channel-delta codec round-trip mismatch");
    }
    Ok(enc)
}

pub fn rebuild_archives(
    clips: &[BossClip],
    plan: &BossPlan,
    rig: &PlayerRig,
    skeleton: usize,
    scale: f32,
) -> Result<RebuiltArchives> {
    let mut main = Vec::new();
    for e in &plan.main_entries {
        let frames: &[Vec<PartPose>] = match e {
            MainEntry::Clip(ci) => &clips[*ci].frames,
            MainEntry::Stub => &clips[plan.map.idle].frames[..1],
        };
        main.push(encode_body(&player_stream(frames, rig, skeleton, scale))?);
    }
    let vic = player_stream(&victory_frames(plan, clips), rig, skeleton, scale);
    let vic_body = encode_body(&vic)?;
    let base = vec![vic_body; plan.base_entries];
    let notes = vec![format!(
        "main ME archive: {} entries, base ME archive: {} victory composites ({} frames)",
        plan.main_entries.len(),
        plan.base_entries,
        victory_frames(plan, clips).len()
    )];
    Ok(RebuiltArchives {
        main: me_slot(&main)?,
        base: me_slot(&base)?,
        notes,
    })
}

// ---------------------------------------------------------------------------
// Equipment sections: swing records

/// Byte length of an action record (head + word-padded stream) at `off`.
fn record_span(d: &[u8], off: usize) -> Result<usize> {
    let parts = *d
        .get(off + PLAYER_HEAD)
        .context("swing record past section")? as usize;
    let frames = *d
        .get(off + PLAYER_HEAD + 1)
        .context("swing record past section")? as usize;
    Ok(PLAYER_HEAD + (2 + parts * frames * 9).div_ceil(4) * 4)
}

/// Replace the swing records of a decoded section record. `new[k]` is the
/// replacement for the record the header word `+4` (k = 0) / `+8` (k = 1)
/// points at.
fn replace_swings(
    decoded: &[u8],
    swing: impl Fn(usize, &[u8]) -> Option<Vec<u8>>,
) -> Result<Vec<u8>> {
    let u32at = |o: usize| legaia_bytes::u32_le(decoded, o).unwrap_or(0) as usize;
    let body_end = u32at(0xC);
    let mut spans: Vec<(usize, usize, Vec<u8>)> = Vec::new(); // (off, old_len, new)
    for (k, w) in [(0usize, 4usize), (1, 8)] {
        let off = u32at(w);
        if off == 0 || spans.iter().any(|s| s.0 == off) {
            continue;
        }
        let len = record_span(decoded, off)?;
        if off + len > body_end {
            bail!("swing record {k} runs past body_end");
        }
        if let Some(new) = swing(k, &decoded[off..off + len]) {
            spans.push((off, len, new));
        }
    }
    if spans.is_empty() {
        return Ok(decoded.to_vec());
    }
    spans.sort_by_key(|s| s.0);
    let map = |off: usize| -> usize {
        let mut o = off as i64;
        for (so, sl, new) in &spans {
            if off >= so + sl {
                o += new.len() as i64 - *sl as i64;
            }
        }
        o as usize
    };
    let mut out = Vec::with_capacity(decoded.len() + 4096);
    let mut cur = 0usize;
    for (so, sl, new) in &spans {
        out.extend_from_slice(&decoded[cur..*so]);
        out.extend_from_slice(new);
        cur = so + sl;
    }
    out.extend_from_slice(&decoded[cur..]);
    let put = |out: &mut [u8], o: usize, v: usize| {
        out[o..o + 4].copy_from_slice(&(v as u32).to_le_bytes())
    };
    for w in [4usize, 8] {
        let off = u32at(w);
        if off != 0 {
            put(&mut out, w, map(off));
        }
    }
    put(&mut out, 0xC, map(body_end));
    let n_attach = i16::from_le_bytes([decoded[0x10], decoded[0x11]]).max(0) as usize;
    for i in 0..n_attach {
        let o = 0x14 + i * 4;
        put(&mut out, o, map(u32at(o)));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Geometry

/// The boss geometry laid into the party band.
pub struct BossGeometry {
    pub channel_geom: BTreeMap<u8, ModelObject>,
    pub section_pools: Vec<Vec<u8>>,
    pub warnings: Vec<String>,
}

fn scale_object(o: &mut ModelObject, s: f32) -> Result<()> {
    if (s - 1.0).abs() < 1e-6 {
        return Ok(());
    }
    for v in o.vertices.iter_mut() {
        for c in v.iter_mut() {
            let x = (*c as f32 * s).round();
            if !(-32768.0..=32767.0).contains(&x) {
                bail!("scaled vertex out of range");
            }
            *c = x as i16;
        }
    }
    Ok(())
}

/// Nominal CLUT row and texture-page column of a monster's own prims: the
/// battle loader (`FUN_80055468`) **adds** `slot + 3` to every prim's CLUT row
/// and `slot + 0x10` to its TSB word, so row 481 / column 5 land on the slot's
/// own CLUT row `484 + slot` and page `5 + slot`.
const NOMINAL_CLUT_ROW: u16 = 481;
const NOMINAL_PAGE_COL: u16 = 5;

/// Point every prim that addresses the next slot at the companion page
/// ([`monster_archive::companion_page`]): its palette id moves to
/// `COMPANION_PAL_BASE + palette`, which `relayout_to_band` reads from the
/// companion's texels and CLUTs. Returns how many prims moved.
fn adopt_companion_prims(objects: &mut [ModelObject]) -> usize {
    let mut n = 0;
    for o in objects.iter_mut() {
        for g in &mut o.groups {
            for p in &mut g.prims {
                if !p.uvs.is_empty()
                    && ((p.cba >> 6) != NOMINAL_CLUT_ROW || (p.tsb & 0xF) != NOMINAL_PAGE_COL)
                {
                    p.cba = (NOMINAL_CLUT_ROW << 6) | (COMPANION_PAL_BASE + (p.cba & 0x3F));
                    n += 1;
                }
            }
        }
    }
    n
}

/// Fallback for a boss whose next-slot prims have no companion page (see
/// [`adopt_companion_prims`]): a small number of them is dropped (the layer
/// under them shows instead); a boss with many keeps them as authored and
/// warns.
fn drop_foreign_prims(objects: &mut [ModelObject], warnings: &mut Vec<String>) {
    let foreign = |p: &legaia_tmd::encode::ModelPrim| {
        !p.uvs.is_empty() && ((p.cba >> 6) != NOMINAL_CLUT_ROW || (p.tsb & 0xF) != NOMINAL_PAGE_COL)
    };
    let (mut bad, mut total) = (0usize, 0usize);
    for o in objects.iter() {
        for g in &o.groups {
            total += g.prims.iter().filter(|p| !p.uvs.is_empty()).count();
            bad += g.prims.iter().filter(|p| foreign(p)).count();
        }
    }
    if bad == 0 {
        return;
    }
    if bad * 20 > total {
        warnings.push(format!(
            "{bad} of {total} textured prims address the next slot's texture page; kept as authored"
        ));
        return;
    }
    for o in objects.iter_mut() {
        for g in &mut o.groups {
            g.prims.retain(|p| !foreign(p));
        }
        o.groups.retain(|g| !g.prims.is_empty());
    }
    warnings.push(format!(
        "{bad} of {total} textured prims address the next slot's texture page and were dropped"
    ));
}

/// Canonical objects whose geometry the section records alias: the last
/// attach bone of every section with an equipment variant shares its primitive
/// region with the variant ([`alias_variant_onto_bone`]), so the loader's
/// TSB relocation reaches those prims twice.
fn aliased_canonical_objects(player_file: &[u8], rig: &PlayerRig) -> Result<Vec<usize>> {
    let pack = battle_data_pack::parse(player_file).context("parse player battle file")?;
    let mut channels: std::collections::BTreeSet<u8> = std::collections::BTreeSet::new();
    for idx in 0..pack.records.len() {
        let decoded = decode_record(player_file, &pack, idx)?;
        let Some(frame_off) = legaia_bytes::u32_le(&decoded.bytes, 0).map(|v| v as usize) else {
            continue;
        };
        let Some(&attach_count) = decoded.bytes.get(frame_off) else {
            continue;
        };
        let attach_count = attach_count as usize;
        let Ok(tmd) =
            legaia_tmd::parse(&decoded.bytes[(frame_off + 0xC).min(decoded.bytes.len())..])
        else {
            continue;
        };
        if super::variant_object(attach_count, tmd.objects.len()).is_some() {
            if let Some(&bone) = decoded.bytes.get(frame_off + attach_count) {
                channels.insert(bone);
            }
        }
    }
    Ok((0..CANONICAL_PARTS)
        .filter(|&c| channels.contains(&rig.channel_for_canonical[c]))
        .collect())
}

pub fn boss_geometry(
    player_file: &[u8],
    rig: &PlayerRig,
    archive: &[u8],
    id: u16,
    char_slot: usize,
    downscale: u32,
    scale: f32,
) -> Result<BossGeometry> {
    let mut warnings = Vec::new();
    let mesh = monster_archive::mesh(archive, id)?
        .ok_or_else(|| anyhow::anyhow!("monster id {id}: empty slot"))?;
    let tmd = legaia_tmd::parse(mesh.tmd_bytes()).context("boss TMD")?;
    let mut objects = decode_model(&tmd, mesh.tmd_bytes())?;
    if objects.len() != CANONICAL_PARTS {
        bail!(
            "monster id {id} has {} parts, need {CANONICAL_PARTS}",
            objects.len()
        );
    }
    let pool = mesh
        .texture_pool_bytes()
        .ok_or_else(|| anyhow::anyhow!("monster id {id}: no texture pool"))?;
    let (mut cluts, indices, width) = monster_pool_texels(pool)?;
    let companion = monster_archive::companion_page(archive, id)?;
    let companion_src = match &companion {
        Some(c) if adopt_companion_prims(&mut objects) > 0 => {
            // Palette ids: the boss's own 15 first, the companion's 16 at
            // `COMPANION_PAL_BASE`.
            cluts.resize(usize::from(COMPANION_PAL_BASE), [0u16; 16]);
            cluts.extend(c.cluts.iter().copied());
            Some((c.indices.as_slice(), c.width))
        }
        _ => {
            drop_foreign_prims(&mut objects, &mut warnings);
            None
        }
    };
    for o in objects.iter_mut() {
        scale_object(o, scale)?;
        compact_object(o);
    }
    let reserved: Vec<u16> =
        legaia_actor_formats::battle_char_assembly::record0_texture_uploads(player_file, 0)?
            .iter()
            .filter(|u| !u.clut.is_empty())
            .flat_map(|u| {
                let first = u.clut_x / 16;
                let n_cols = (u.clut.len() as u16).div_ceil(16);
                (first..first + n_cols).collect::<Vec<u16>>()
            })
            .collect();
    let aliased = aliased_canonical_objects(player_file, rig)?;
    let layout = relayout_to_band(
        &mut objects,
        &cluts,
        &indices,
        width,
        companion_src,
        true,
        &aliased,
        &reserved,
        None,
        downscale,
        char_slot,
        &mut warnings,
    )?;
    let mut channel_geom = BTreeMap::new();
    for (c, o) in objects.into_iter().enumerate() {
        channel_geom.insert(rig.channel_for_canonical[c], o);
    }
    Ok(BossGeometry {
        channel_geom,
        section_pools: layout.section_pools,
        warnings,
    })
}

// ---------------------------------------------------------------------------
// The whole player file

/// Replacement for the swing record `old` (head + stream) playing `clip`.
fn swing_entry(old: &[u8], clip: &BossClip, rig: &PlayerRig, scale: f32) -> Vec<u8> {
    let parts = old[PLAYER_HEAD] as usize;
    let mut e = old[..PLAYER_HEAD].to_vec();
    apply_clip_head(&mut e, clip, clip.len(), Kind::Attack);
    clamp_loop(&mut e, clip.len());
    e.extend_from_slice(&player_stream(&clip.frames, rig, parts, scale));
    e
}

/// The rebuilt player file plus bookkeeping.
pub struct Bossized {
    /// Retail-shaped file (header + record[0] + table below `0x8000`,
    /// records chained from `0x8000`); not padded to any entry length.
    pub file: Vec<u8>,
    pub warnings: Vec<String>,
    /// Persistent-buffer demand of the worst equipment pick, new vs retail.
    pub worst_member_bytes: usize,
    pub retail_worst_member_bytes: usize,
    pub record0_stream_bytes: usize,
    /// record[0] offsets of the [`BossPlan::stage_pool`] entries.
    pub pool_offsets: Vec<usize>,
}

/// Swing slot (`0xC..=0xF`) a section's `k`th swing record fills.
fn swing_slot(section: usize, k: usize) -> Option<usize> {
    match (section, k) {
        (2, 0) => Some(0xC),
        (3, 0) => Some(0xD),
        (4, 0) => Some(0xE),
        (4, 1) => Some(0xF),
        _ => None,
    }
}

/// Install a rewritten record[0] into a rebuilt file's prologue: header
/// words, the LZS stream, and the descriptor table moved up behind it.
fn install_record0(
    file: &mut [u8],
    old_table_off: usize,
    n_records: usize,
    data_base: usize,
    new_r0: &[u8],
    hdr: [usize; 3],
) -> Result<usize> {
    let table_len = (n_records + 1) * 12;
    let table = file
        .get(old_table_off..old_table_off + table_len)
        .context("descriptor table past the prologue")?
        .to_vec();
    let stream = legaia_lzs::compress_optimal(new_r0);
    let back = legaia_lzs::decompress(&stream, new_r0.len())?;
    if back != new_r0 {
        bail!("record[0] LZS round-trip mismatch");
    }
    let r0_end = 0x10 + stream.len();
    let new_table_off = (r0_end + 0xF) & !0xF;
    if new_table_off + table_len > data_base {
        bail!(
            "record[0] ({} bytes compressed) leaves no room for the descriptor table below {data_base:#x}",
            stream.len()
        );
    }
    file[..data_base].fill(0);
    let put =
        |f: &mut [u8], o: usize, v: usize| f[o..o + 4].copy_from_slice(&(v as u32).to_le_bytes());
    put(file, 0, new_table_off);
    put(file, 4, hdr[0]);
    put(file, 8, hdr[1]);
    put(file, 0xC, hdr[2]);
    file[0x10..r0_end].copy_from_slice(&stream);
    file[new_table_off..new_table_off + table_len].copy_from_slice(&table);
    Ok(stream.len())
}

#[allow(clippy::too_many_arguments)]
pub fn bossize_player_file(
    player_file: &[u8],
    rig: &PlayerRig,
    archive: &[u8],
    id: u16,
    char_slot: usize,
    plan: &BossPlan,
    clips: &[BossClip],
    downscale: u32,
    scale: f32,
) -> Result<Bossized> {
    let pack = battle_data_pack::parse(player_file).context("parse player battle file")?;
    let rec0 = Record0::parse(player_file)?;
    let geom = boss_geometry(player_file, rig, archive, id, char_slot, downscale, scale)?;
    let warnings = geom.warnings.clone();

    let mut new_records: Vec<(u32, Vec<u8>)> = Vec::new();
    let up_melee: std::cell::RefCell<Option<Vec<u8>>> = std::cell::RefCell::new(None);
    let mut section = 0usize;
    let mut anchor_section: Option<usize> = None;
    let mut max_body = [0usize; SECTION_COUNT];
    let mut max_len = [0usize; SECTION_COUNT];
    let mut old_body = [0usize; SECTION_COUNT];
    let mut old_len = [0usize; SECTION_COUNT];
    for (idx, rec) in pack.records.iter().enumerate() {
        let decoded = decode_record(player_file, &pack, idx)
            .with_context(|| format!("decode record {idx}"))?;
        let pool = geom
            .section_pools
            .get(section)
            .ok_or_else(|| anyhow::anyhow!("record {idx}: section {section} out of range"))?;
        old_body[section] =
            old_body[section].max(legaia_bytes::u32_le(&decoded.bytes, 0xC).unwrap_or(0) as usize);
        old_len[section] = old_len[section].max(decoded.bytes.len());
        let rewritten = rewrite_section_record(
            &decoded.bytes,
            &geom.channel_geom,
            pool,
            section,
            &mut anchor_section,
        )
        .with_context(|| format!("rewrite record {idx} (id {:#x})", rec.id))?;
        let rewritten = replace_swings(&rewritten, |k, old| {
            let slot = swing_slot(section, k)?;
            let clip = plan.map.swings[slot - 0xC];
            if slot == 0xF
                && let Some(up) = plan.up_melee
                && up_melee.borrow().is_none()
            {
                // The melee the fireball displaced, in the host's own swing
                // shape (power run and rules from its retail High swing).
                *up_melee.borrow_mut() = Some(swing_entry(old, &clips[up], rig, scale));
            }
            let mut entry = swing_entry(old, &clips[clip], rig, scale);
            if plan.fireball_swing == Some(clip) {
                let n = clips[clip].len();
                set_loop(
                    &mut entry,
                    FIREBALL_LOOP.0,
                    FIREBALL_LOOP.1,
                    FIREBALL_LOOP.2,
                );
                clamp_loop(&mut entry, n);
            }
            Some(entry)
        })
        .with_context(|| format!("swap swing records of record {idx}"))?;
        max_body[section] =
            max_body[section].max(legaia_bytes::u32_le(&rewritten, 0xC).unwrap_or(0) as usize);
        max_len[section] = max_len[section].max(rewritten.len());
        new_records.push((rec.id, rewritten));
        if rec.id == 0 {
            section += 1;
        }
    }
    if section != SECTION_COUNT {
        bail!("descriptor chain closed after {section} sections");
    }

    let up_melee = up_melee.into_inner();
    if plan.up_melee.is_some() && up_melee.is_none() {
        bail!("no High swing record found to template the Up melee row");
    }
    let (new_r0, hdr, pool_offsets) =
        rebuild_record0(&rec0, clips, plan, rig, scale, up_melee.as_deref())?;
    let n_records = new_records.len();
    let mut file =
        rebuild_player_file_unbounded(player_file, pack.table_offset, pack.data_base, new_records)?;
    let stream_bytes = install_record0(
        &mut file,
        pack.table_offset,
        n_records,
        pack.data_base,
        &new_r0,
        hdr,
    )?;
    let worst = |clut_a: usize, body: &[usize; SECTION_COUNT], len: &[usize; SECTION_COUNT]| {
        clut_a + body[..SECTION_COUNT - 1].iter().sum::<usize>() + len[SECTION_COUNT - 1]
    };
    Ok(Bossized {
        file,
        warnings,
        worst_member_bytes: worst(hdr[0], &max_body, &max_len),
        retail_worst_member_bytes: worst(rec0.clut_a, &old_body, &old_len),
        record0_stream_bytes: stream_bytes,
        pool_offsets,
    })
}
