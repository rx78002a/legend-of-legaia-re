//! A swapped host's monster voice: the boss's own grunts, cries and steps.
//!
//! A monster's clip sound cues (`entry + 0x54`) name rows of the battle's
//! runtime descriptor bank (`bse.dat`, PROT 0888: cue `id + 0x19C`) whose
//! program / tone live in the monster's own single-program VAB inside
//! `monster.snd` (PROT 0891, bank `monster id - 1`). That bank is only in SPU
//! RAM when the monster is in the formation, so a host that plays the same cue
//! ids is silent (or, for ids `>= 0xC8`, a party-skewed misroute).
//!
//! The party's own voice is program 7 of the always-resident battle bank
//! (PROT 0869, VAB slot 2): eight tones, two of which (`t2` and the shared
//! bodies) are placeholders. The host's cues reach its tones through static
//! descriptor rows `0x51..=0x56` (tones 0 and 1, 3, 4, 5) and the party band
//! rows `0xA7` / `0xA8` (tones 6 and 7). This module overwrites those tones in
//! place with the boss's samples (timbre fields, ADPCM body) and the rows'
//! note levels with the boss rows', then rewrites the boss clips' cue tracks to
//! the host cue ids that now reach them. Nothing moves or grows (the same
//! in-place rules as `delilas_voice`): a longer sample is truncated at a block
//! boundary with the end flag forced.

use anyhow::{Context, Result, bail};
use legaia_container_formats::bse_bank;
use legaia_disc_patch::disc::DiscPatcher;
use legaia_party_swap::playerize::boss::BossClip;

/// PROT entry of the always-resident battle voice / SFX bank.
const BATTLE_BANK_ENTRY: usize = 869;
/// PROT entry of the battle runtime descriptor bank (`bse.dat`).
const BSE_ENTRY: usize = 888;
/// PROT entry of `monster.snd`.
const MONSTER_SND_ENTRY: usize = 891;
/// The host's voice program in the battle bank (Vahn's).
const HOST_PROGRAM: usize = 7;
/// Static SFX descriptor table in `SCUS_942.54`, 8 bytes per id.
const STATIC_TABLE_VA: u32 = 0x8006_F198;
/// SPU-ADPCM block size.
const BLOCK: usize = 16;
/// Cue ids at or above this are runtime-bank voice rows for a monster.
const FIRST_VOICE_CUE: u16 = 0x64;

/// How a host cue reaches its descriptor row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    /// A row of the static table (`SCUS_942.54`), ids `0x48..0x64` unchanged.
    Static(usize),
    /// A row of the runtime bank (`bse.dat`) for the party band `0xA7..`.
    Runtime(usize),
}

/// A tone of the host program a boss sample can take, and the cue that plays it.
#[derive(Debug, Clone, Copy)]
struct Slot {
    cue: u16,
    row: Row,
    tone: usize,
}

/// Vahn's program-7 tones with a voice of their own, by the cue that fires them.
/// (Tone 2 is a silent placeholder.)
const SLOTS: [Slot; 7] = [
    Slot {
        cue: 0x51,
        row: Row::Static(0x51),
        tone: 0,
    },
    Slot {
        cue: 0x52,
        row: Row::Static(0x52),
        tone: 1,
    },
    Slot {
        cue: 0x54,
        row: Row::Static(0x54),
        tone: 3,
    },
    Slot {
        cue: 0x55,
        row: Row::Static(0x55),
        tone: 4,
    },
    Slot {
        cue: 0x56,
        row: Row::Static(0x56),
        tone: 5,
    },
    Slot {
        cue: 0xA7,
        row: Row::Runtime(0x43),
        tone: 6,
    },
    Slot {
        cue: 0xA8,
        row: Row::Runtime(0x44),
        tone: 7,
    },
];

/// The geometry of one VAB inside a buffer.
struct VabView {
    header_offset: usize,
    page_programs: Vec<usize>,
    page_tones: Vec<usize>,
    /// `(byte_offset, size)` per VAG body, 0-indexed by `vag - 1`.
    vag_spans: Vec<(usize, usize)>,
}

fn view(buf: &[u8], offset: usize) -> Result<VabView> {
    let report = legaia_vab::parse(buf, offset).context("parse VAB")?;
    let populated: Vec<usize> = report
        .programs
        .iter()
        .enumerate()
        .filter(|(_, p)| p.tones > 0)
        .map(|(i, _)| i)
        .collect();
    if populated.len() != report.tones.len() {
        bail!(
            "VAB at {offset:#x}: {} populated programs but {} tone pages",
            populated.len(),
            report.tones.len()
        );
    }
    let page_tones = populated
        .iter()
        .map(|&p| report.programs[p].tones as usize)
        .collect();
    Ok(VabView {
        header_offset: report.header_offset,
        page_programs: populated,
        page_tones,
        vag_spans: report
            .vag_samples
            .iter()
            .map(|s| (s.byte_offset, s.size))
            .collect(),
    })
}

impl VabView {
    fn page_of(&self, program: usize) -> Option<usize> {
        self.page_programs.iter().position(|&p| p == program)
    }

    fn tone_offset(&self, page: usize, t: usize) -> usize {
        self.header_offset
            + legaia_vab::VAB_HEADER_SIZE
            + legaia_vab::PROGRAMS_TABLE_SIZE
            + (page * legaia_vab::TONES_PER_PROGRAM + t) * legaia_vab::TONE_SIZE
    }

    fn tone_vag(&self, buf: &[u8], page: usize, t: usize) -> usize {
        let o = self.tone_offset(page, t);
        i16::from_le_bytes(buf[o + 22..o + 24].try_into().unwrap()).max(0) as usize
    }

    /// The true ADPCM span of a VAG: the parser's raw spans sit four bytes
    /// early of the block grid.
    fn body(&self, buf: &[u8], vag: usize) -> Option<(usize, usize)> {
        let (o, n) = *self.vag_spans.get(vag.checked_sub(1)?)?;
        let end = (o + 4 + n).min(buf.len());
        (o + 4 < end).then_some((o + 4, end - (o + 4)))
    }

    /// Is `vag` read by a tone outside `page`?
    fn shared_outside(&self, buf: &[u8], page: usize, vag: usize) -> bool {
        self.page_tones
            .iter()
            .enumerate()
            .any(|(p, &tones)| p != page && (0..tones).any(|t| self.tone_vag(buf, p, t) == vag))
    }
}

/// `monster.snd`'s VAB of `monster_id` (`tbl[id - 1]`, the VAB four bytes in).
fn monster_vab_offset(snd: &[u8], monster_id: u16) -> Result<usize> {
    let count = u32::from_le_bytes(
        snd.get(4..8)
            .context("monster.snd too short")?
            .try_into()
            .unwrap(),
    ) as usize;
    let id = usize::from(monster_id)
        .checked_sub(1)
        .context("monster id 0")?;
    if id >= count {
        bail!("monster id {monster_id} past monster.snd count {count}");
    }
    let sec = u32::from_le_bytes(snd[8 + id * 4..12 + id * 4].try_into().unwrap()) as usize;
    let base = sec * 0x800;
    for probe in [base + 4, base] {
        if snd.len() > probe + 4
            && u32::from_le_bytes(snd[probe..probe + 4].try_into().unwrap())
                == legaia_vab::VAB_MAGIC
        {
            return Ok(probe);
        }
    }
    bail!("monster id {monster_id}: no VAB at monster.snd sector {sec}")
}

/// Copy `src` over `dst` in place; a longer source is cut on a block boundary
/// with the end flag on and repeat off. Returns the bytes kept.
fn overwrite_vag(dst: &mut [u8], src: &[u8]) -> usize {
    let n = src.len().min(dst.len()) / BLOCK * BLOCK;
    if n == 0 {
        return 0;
    }
    dst[..n].copy_from_slice(&src[..n]);
    if n < src.len() {
        let flags = &mut dst[n - BLOCK + 1];
        *flags = (*flags & !0x02) | 0x01;
    }
    n
}

/// One boss cue, and where it ended up.
#[derive(Debug, Clone)]
pub struct VoiceMove {
    pub from: u16,
    pub to: u16,
    /// Source sample bytes and the bytes that fit.
    pub bytes: (usize, usize),
}

/// What [`plan`] decided, with the patched buffers ready to [`commit`].
pub struct VoicePlan {
    pub moves: Vec<VoiceMove>,
    pub dropped: Vec<u16>,
    bank: Vec<u8>,
    bse: Vec<u8>,
    /// `(SCUS file offset, bytes)` of the static rows rewritten.
    static_rows: Vec<(usize, [u8; 8])>,
}

impl VoicePlan {
    pub fn notes(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .moves
            .iter()
            .map(|m| {
                format!(
                    "voice cue {:#x} -> host cue {:#x} ({} of {} sample bytes)",
                    m.from, m.to, m.bytes.1, m.bytes.0
                )
            })
            .collect();
        if !self.dropped.is_empty() {
            v.push(format!(
                "voice cues without a free host tone, dropped: {:x?}",
                self.dropped
            ));
        }
        v
    }
}

/// How much a clip's cue matters, by its animation tag.
fn tag_weight(tag: u8) -> u32 {
    match tag {
        2..=4 => 5,       // flinches, knockdown
        0x0D..=0x10 => 4, // attacks
        0x23 => 3,        // cast
        1 => 2,           // walk
        _ => 1,
    }
}

/// Plan the voice of `monster`: assign its voice cues to host tones, patch the
/// battle bank and rows in memory, and rewrite the clips' cue tracks to the
/// host cue ids (a cue with no free tone is dropped). `None` when the monster
/// has no voice bank or no voice cue.
pub fn plan(
    patcher: &DiscPatcher,
    monster: u16,
    clips: &mut [BossClip],
) -> Result<Option<VoicePlan>> {
    // the boss's voice cues, weighted by use
    let mut weight: std::collections::BTreeMap<u16, u32> = Default::default();
    let mut own_cues: Vec<u16> = Vec::new();
    for c in clips.iter() {
        for o in (0x54..0x74).step_by(4) {
            let cue = u16::from_le_bytes([c.head[o + 2], c.head[o + 3]]);
            if cue == 0 {
                continue;
            }
            own_cues.push(cue);
            if cue >= FIRST_VOICE_CUE {
                *weight.entry(cue).or_default() += tag_weight(c.tag);
            }
        }
    }
    if weight.is_empty() {
        return Ok(None);
    }
    let snd = patcher
        .read_entry(MONSTER_SND_ENTRY)
        .context("read monster.snd")?;
    let Ok(src_off) = monster_vab_offset(&snd, monster) else {
        return Ok(None);
    };
    let src_view = view(&snd, src_off)?;
    let bse = patcher.read_entry(BSE_ENTRY).context("read bse.dat")?;
    let mut bank = patcher
        .read_entry(BATTLE_BANK_ENTRY)
        .context("read the battle bank (PROT 0869)")?;
    let bank_off = *legaia_vab::find_vabs(&bank)
        .first()
        .context("PROT 0869 carries no VAB")?;
    let bank_view = view(&bank, bank_off)?;
    let page = bank_view
        .page_of(HOST_PROGRAM)
        .context("host voice program not populated")?;
    let scus = patcher
        .read_named_file("SCUS_942.54")
        .context("read SCUS_942.54")?;
    let table_off = legaia_game_tables::item_names::file_offset_for_va(&scus, STATIC_TABLE_VA)
        .context("static descriptor table")?;
    let mut bse_out = bse.clone();

    // the host tones free to take a sample: audible, page-exclusive body, and
    // the boss does not use the cue id itself
    struct Free {
        slot: Slot,
        capacity: usize,
    }
    let mut free: Vec<Free> = Vec::new();
    for slot in SLOTS {
        if own_cues.contains(&slot.cue) {
            continue;
        }
        let o = bank_view.tone_offset(page, slot.tone);
        if bank[o + 2] == 0 {
            continue; // silent placeholder
        }
        let vag = bank_view.tone_vag(&bank, page, slot.tone);
        if bank_view.shared_outside(&bank, page, vag) {
            continue;
        }
        let Some((_, cap)) = bank_view.body(&bank, vag) else {
            continue;
        };
        free.push(Free {
            slot,
            capacity: cap,
        });
    }

    // the boss samples by cue
    struct Source {
        cue: u16,
        weight: u32,
        timbre: [u8; 20],
        level: u8,
        body: Vec<u8>,
    }
    let mut sources: Vec<Source> = Vec::new();
    let mut dropped: Vec<u16> = Vec::new();
    for (&cue, &w) in &weight {
        let Some(row) = bse_bank::record_for_cue(&bse, cue + 0x19C) else {
            dropped.push(cue);
            continue;
        };
        let Some(src_page) = src_view.page_of(usize::from(row.program)) else {
            dropped.push(cue);
            continue;
        };
        let t = usize::from(row.tone);
        if t >= src_view.page_tones[src_page] {
            dropped.push(cue);
            continue;
        }
        let so = src_view.tone_offset(src_page, t);
        let vag = src_view.tone_vag(&snd, src_page, t);
        let Some((bo, bn)) = src_view.body(&snd, vag) else {
            dropped.push(cue);
            continue;
        };
        sources.push(Source {
            cue,
            weight: w,
            timbre: snd[so..so + 20].try_into().unwrap(),
            level: row.level,
            body: snd[bo..bo + bn].to_vec(),
        });
    }
    sources.sort_by_key(|s| (std::cmp::Reverse(s.weight), s.cue));

    let mut moves = Vec::new();
    let mut static_rows: Vec<(usize, [u8; 8])> = Vec::new();
    for s in sources {
        // the smallest free tone that holds the sample whole, else the largest
        let pick = free
            .iter()
            .enumerate()
            .filter(|(_, f)| f.capacity >= s.body.len())
            .min_by_key(|(_, f)| f.capacity)
            .map(|(i, _)| i)
            .or_else(|| {
                free.iter()
                    .enumerate()
                    .max_by_key(|(_, f)| f.capacity)
                    .map(|(i, _)| i)
            });
        let Some(i) = pick else {
            dropped.push(s.cue);
            continue;
        };
        let f = free.swap_remove(i);
        let o = bank_view.tone_offset(page, f.slot.tone);
        bank[o..o + 20].copy_from_slice(&s.timbre);
        let vag = bank_view.tone_vag(&bank, page, f.slot.tone);
        let (bo, bn) = bank_view.body(&bank, vag).context("host body")?;
        let kept = overwrite_vag(&mut bank[bo..bo + bn], &s.body);
        // the row: the boss row's note level, one voice
        match f.slot.row {
            Row::Static(id) => {
                let at = table_off + id * 8;
                let mut row: [u8; 8] = scus[at..at + 8].try_into().unwrap();
                row[2] = s.level;
                row[3] = (row[3] & !0x1F) | 1;
                static_rows.push((at, row));
            }
            Row::Runtime(idx) => {
                let at = 4 + idx * 8;
                bse_out[at + 2] = s.level;
                bse_out[at + 3] = (bse_out[at + 3] & !0x1F) | 1;
            }
        }
        moves.push(VoiceMove {
            from: s.cue,
            to: f.slot.cue,
            bytes: (s.body.len(), kept),
        });
    }

    // rewrite the clips' cue tracks: voice cues become host cues or go
    for c in clips.iter_mut() {
        let mut kept: Vec<[u8; 4]> = Vec::new();
        for o in (0x54..0x74).step_by(4) {
            let pair: [u8; 4] = c.head[o..o + 4].try_into().unwrap();
            let cue = u16::from_le_bytes([pair[2], pair[3]]);
            if cue == 0 {
                break;
            }
            if cue < FIRST_VOICE_CUE {
                kept.push(pair);
            } else if let Some(m) = moves.iter().find(|m| m.from == cue) {
                kept.push([pair[0], pair[1], m.to as u8, (m.to >> 8) as u8]);
            }
        }
        c.head[0x54..0x74].fill(0);
        for (k, pair) in kept.iter().take(8).enumerate() {
            c.head[0x54 + k * 4..0x58 + k * 4].copy_from_slice(pair);
        }
    }
    Ok(Some(VoicePlan {
        moves,
        dropped,
        bank,
        bse: bse_out,
        static_rows,
    }))
}

/// Write a planned voice onto the disc.
pub fn commit(patcher: &mut DiscPatcher, plan: &VoicePlan) -> Result<()> {
    patcher
        .patch_prot_entry(BATTLE_BANK_ENTRY, 0, &plan.bank)
        .context("write the battle bank")?;
    patcher
        .patch_prot_entry(BSE_ENTRY, 0, &plan.bse)
        .context("write bse.dat")?;
    for (at, row) in &plan.static_rows {
        patcher
            .patch_named_file("SCUS_942.54", *at as u64, row)
            .context("write a static descriptor row")?;
    }
    Ok(())
}
