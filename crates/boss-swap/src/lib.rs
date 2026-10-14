//! Play as a boss: a party slot's **battle** model, motions and (later) stats
//! and voices come from a monster of the archive. Field models are untouched.
//!
//! The kernels live in `legaia_party_swap::playerize::boss`; this crate is the
//! disc driver on top of them.

pub mod direct_cast;
pub mod render;

use anyhow::{Context, Result, bail};
use legaia_actor_formats::{battle_char_assembly, me_archive};
use legaia_disc_patch::disc::DiscPatcher;
use legaia_party_swap::playerize::boss;
use legaia_party_swap::{RIG_VAHN_GALA, winpose};

/// PROT entry of the monster archive.
pub const MONSTER_ARCHIVE_ENTRY: usize = 867;
/// PROT entry of `readef.DAT`.
pub const READEF_ENTRY: usize = 894;
/// Vahn's player battle file.
pub const VAHN_ENTRY: usize = 863;

#[derive(Debug, Clone)]
pub struct Options {
    /// Monster id of the boss (1-based archive id).
    pub monster: u16,
    /// Uniform scale applied to the boss geometry and pose translations.
    pub scale: f32,
    /// The art record (by inline name) that plays the boss's cast - its
    /// fireball - instead of a melee move. `None` keeps melee in every art.
    pub fireball_art: Option<String>,
    /// Direction swing `0xF` (Up) plays the boss's cast instead of a melee
    /// move: the fireball is one D-pad press. A stage-1 variant that needs
    /// `direct_cast` off (the hooks key on art constants, not swings).
    pub fireball_swing: bool,
    /// Stage 2 hooks ([`direct_cast`]): the cast art plays in place with no
    /// walk-up, no leading arrows or swings, and its own spell's power.
    pub direct_cast: bool,
    /// Mute the host character's own voice banks (arts shout, fanfares,
    /// swing grunt, victory barks).
    pub mute_host_voice: bool,
    /// Replace the item / spell / defeat pose (record[0] slot 9) and the Spirit
    /// command's clip with the boss idle, instead of the boss's knockdown and
    /// multi-hit move.
    pub calm_support: bool,
    /// The fireball projectile keeps the monster spell's own damage; `false`
    /// zeroes its power so only the art's hits damage.
    pub fireball_damage: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            monster: 76,
            scale: 0.8,
            fireball_art: Some("Somersault".to_string()),
            fireball_swing: false,
            direct_cast: true,
            mute_host_voice: true,
            calm_support: true,
            fireball_damage: true,
        }
    }
}

#[derive(Debug, Default)]
pub struct Report {
    pub notes: Vec<String>,
}

/// Apply the boss swap to Vahn's slot. Writes through `patcher`.
pub fn apply(patcher: &mut DiscPatcher, opts: &Options) -> Result<Report> {
    let mut report = Report::default();
    let archive = patcher
        .read_entry_footprint(MONSTER_ARCHIVE_ENTRY)
        .context("read monster archive")?;
    let player = patcher
        .read_entry_footprint(VAHN_ENTRY)
        .context("read Vahn's player file")?;
    let entry_len = patcher.read_entry(VAHN_ENTRY)?.len();
    let readef = patcher
        .read_entry_footprint(READEF_ENTRY)
        .context("read readef.DAT")?;

    let rig = RIG_VAHN_GALA;
    let char_slot = 0usize;
    let main_slot = battle_char_assembly::art_me_slot(char_slot, false);
    let base_slot = battle_char_assembly::art_me_slot(char_slot, true);
    let slot_bytes = |s: usize| -> Result<&[u8]> {
        readef
            .get(s * winpose::READEF_SLOT..(s + 1) * winpose::READEF_SLOT)
            .with_context(|| format!("readef slot {s} out of range"))
    };
    let main_n = me_archive::parse(slot_bytes(main_slot)?)?.len();
    let base_n = me_archive::parse(slot_bytes(base_slot)?)?.len();

    let mut clips = boss::boss_clips(&archive, opts.monster)?;
    let rec0 = boss::Record0::parse(&player)?;
    let fireball = if opts.fireball_art.is_some() {
        Some(boss::add_fireball_clip(&mut clips)?)
    } else {
        None
    };
    let calm = if opts.calm_support {
        Some(boss::add_calm_clips(&mut clips)?)
    } else {
        None
    };
    let mut map = boss::default_clip_map(&clips)?;
    if let Some((pose, spirit)) = calm {
        map.slots.insert(9, pose);
        map.spirit = Some(spirit);
    }
    let swing_fireball = if opts.fireball_swing {
        let clip = boss::add_fireball_clip(&mut clips)?;
        map.swings[3] = clip;
        Some(clip)
    } else {
        None
    };
    let fb = fireball.zip(opts.fireball_art.as_deref());
    let mut plan = boss::plan_boss_with(&rec0, map, main_n, base_n, fb)?;
    plan.fireball_swing = swing_fireball;
    if opts.fireball_swing && opts.direct_cast {
        bail!(
            "--fireball-swing needs --no-direct-cast: the direct-cast hooks key on art constants"
        );
    }
    if opts.direct_cast
        && let Some((pose, _)) = calm
    {
        // The art starter (queue byte 0x19 / 0x1A) plays the boss idle: the
        // walk it played would carry the host forward before a cast.
        let slot = STARTER_ENTRY;
        if main_n > slot {
            plan.main_entries[slot] = boss::MainEntry::Clip(pose);
            for a in plan
                .arts
                .iter_mut()
                .filter(|a| !a.base && a.name == "Starter")
            {
                a.new_source = slot;
            }
        }
    }
    report.notes.push(format!(
        "boss #{}: {} clips; art records: {}, main ME entries: {main_n}, base: {base_n}",
        opts.monster,
        clips.len(),
        plan.arts.len()
    ));

    let skeleton = rec0
        .stream_shape(0)
        .map(|s| s.0)
        .context("no idle in record[0]")?;
    let mut done = None;
    let mut last_err = None;
    for downscale in [1u32, 2, 4] {
        match boss::bossize_player_file(
            &player,
            &rig,
            &archive,
            opts.monster,
            char_slot,
            &plan,
            &clips,
            downscale,
            opts.scale,
        ) {
            Ok(b) => {
                if downscale > 1 {
                    report
                        .notes
                        .push(format!("texture at 1/{downscale} resolution"));
                }
                done = Some(b);
                break;
            }
            Err(e) => last_err = Some(e),
        }
    }
    let b = done.ok_or_else(|| last_err.unwrap())?;
    report.notes.extend(b.warnings.iter().cloned());
    report.notes.push(format!(
        "record[0] stream {} bytes; member buffer worst case {:#x} (retail {:#x}, limit {:#x})",
        b.record0_stream_bytes,
        b.worst_member_bytes,
        b.retail_worst_member_bytes,
        boss::MEMBER_ALLOC
    ));
    if b.worst_member_bytes > boss::MEMBER_ALLOC {
        bail!(
            "the rebuilt player file needs {:#x} bytes of member buffer, the loader allocates {:#x}",
            b.worst_member_bytes,
            boss::MEMBER_ALLOC
        );
    }
    if b.file.len() <= entry_len {
        let mut file = b.file;
        file.resize(entry_len, 0);
        patcher.patch_prot_entry(VAHN_ENTRY, 0, &file)?;
        report.notes.push(format!(
            "player file written in place ({} bytes)",
            file.len()
        ));
    } else {
        let place = patcher.annex_player_file(VAHN_ENTRY, &b.file)?;
        report.notes.push(format!(
            "player file {} bytes > entry {}; records parked in the DMY annex at LBA {} ({} sectors)",
            b.file.len(),
            entry_len,
            place.lba,
            place.sectors
        ));
    }

    let arch = boss::rebuild_archives(&clips, &plan, &rig, skeleton, opts.scale)?;
    report.notes.extend(arch.notes.iter().cloned());
    patcher.patch_prot_entry(
        READEF_ENTRY,
        (main_slot * winpose::READEF_SLOT) as u64,
        &arch.main,
    )?;
    patcher.patch_prot_entry(
        READEF_ENTRY,
        (base_slot * winpose::READEF_SLOT) as u64,
        &arch.base,
    )?;
    let cast_id = if opts.fireball_art.is_some() || opts.fireball_swing {
        Some(route_fireball_move(
            patcher,
            opts.monster,
            opts.fireball_damage,
            !opts.direct_cast,
            &mut report,
        )?)
    } else {
        None
    };
    if opts.direct_cast
        && let (Some(spell), Some(art)) = (
            cast_id,
            plan.arts.iter().find(|a| a.fireball).map(|a| a.index),
        )
    {
        let constant = direct_cast::TABLE_BASE + art as u8;
        let fresh = direct_cast::install(patcher, &[direct_cast::CastArt { constant, spell }])?;
        report.notes.push(format!(
            "direct cast hooks {}: art constant {constant:#x} casts spell {spell:#x} in place",
            if fresh {
                "installed"
            } else {
                "already in place"
            }
        ));
    }
    if opts.mute_host_voice {
        mute_vahn_voice(patcher, &mut report)?;
    }
    Ok(report)
}

/// Main ME entry the art starters stream from when the cast hooks are on
/// (the first stub past the fireball and multi-hit entries).
const STARTER_ENTRY: usize = 8;

/// Vahn's voice lives in CD-XA files, not in the battle file: the arts shout
/// bank, the Hyper / Super fanfare and Seru-magic fanfare banks, the swing
/// grunt channel of the shared grunt bank and the victory-bark channels.
/// Each cue still fires; the sectors decode to silence.
fn mute_vahn_voice(patcher: &mut DiscPatcher, report: &mut Report) -> Result<()> {
    for file in ["XA/XA1.XA", "XA/XA2.XA", "XA/XA27.XA"] {
        let n = patcher
            .silence_xa_file(file)
            .with_context(|| format!("mute {file}"))?;
        report.notes.push(format!("{file} muted ({n} sectors)"));
    }
    for (file, channels) in [("XA/XA30.XA", &[0u8][..]), ("XA/XA21.XA", &[2, 3][..])] {
        let n = patcher
            .silence_xa_channels(file, channels)
            .with_context(|| format!("mute {file} channels {channels:?}"))?;
        report
            .notes
            .push(format!("{file} channels {channels:?} muted ({n} sectors)"));
    }
    Ok(())
}

/// PROT entry of the battle-action overlay and the file offset of its move-id
/// to move-power-record map (`0x80` bytes, one per move id).
const BATTLE_OVERLAY_ENTRY: usize = 898;
const MOVE_POWER_MAP: usize = 0x2664B;
/// The move id the fireball art's queue starts with: a High swing byte, which
/// no retail record maps (party arts take no power from the table).
const FIREBALL_MOVE_ID: usize = 0x0F;

/// Layout of the move-power table next to the id map.
const MOVE_POWER_TABLE: usize = 0x26744;
const MOVE_POWER_STRIDE: usize = 26;
/// Move id `0x2C` (the unused "Freeze Thunder" enemy spell) owns record 22,
/// which no monster reads: the fireball's own copy lives there.
const SPARE_MOVE_ID: usize = 0x2C;

/// Settle which move-power record the fireball plays and return the move id
/// that selects it.
///
/// With the direct-cast hooks the move id is a spell id the id map already
/// resolves: the boss's own magic, or - with `keep_damage` off - the spare id
/// `0x2C`, whose record is overwritten with a copy of the boss's
/// special-attack record (launch and contact effects, homing, cue) with the
/// damage power zeroed. The clip's terminator installs it and the projectile
/// spawns, while the art's own hit events do the damage; a kill by the
/// projectile's damage path skips the victim's reaction pose and the enemy
/// vanishes instead of collapsing.
///
/// Without the hooks (`map_swing_byte`) the record is reached through the
/// queue head instead, so the High swing byte `0x0F` is mapped to it.
fn route_fireball_move(
    patcher: &mut DiscPatcher,
    monster: u16,
    keep_damage: bool,
    map_swing_byte: bool,
    report: &mut Report,
) -> Result<u8> {
    let archive = patcher.read_entry_footprint(MONSTER_ARCHIVE_ENTRY)?;
    let block = legaia_actor_formats::monster_archive::decode_block(&archive, monster)?
        .context("empty monster slot")?;
    let spell = *block.get(0x21).context("monster record too short")? as usize;
    let overlay = patcher.read_entry(BATTLE_OVERLAY_ENTRY)?;
    let map = overlay
        .get(MOVE_POWER_MAP..MOVE_POWER_MAP + 0x80)
        .context("battle overlay shorter than its move map")?;
    if spell >= map.len() || map[spell] == 0 {
        bail!("monster magic id {spell:#x} has no move-power record to borrow");
    }
    if map_swing_byte && map[FIREBALL_MOVE_ID] != 0 {
        bail!("move id {FIREBALL_MOVE_ID:#x} is already mapped to a power record");
    }
    if keep_damage {
        let id = map[spell];
        if map_swing_byte {
            patcher.patch_prot_entry(
                BATTLE_OVERLAY_ENTRY,
                (MOVE_POWER_MAP + FIREBALL_MOVE_ID) as u64,
                &[id],
            )?;
            report.notes.push(format!(
                "move id {FIREBALL_MOVE_ID:#x} -> move-power record {id} (monster magic {spell:#x})"
            ));
        } else {
            report.notes.push(format!(
                "cast spell {spell:#x} -> move-power record {id} (the boss's own)"
            ));
        }
        return Ok(spell as u8);
    }
    let spare = map[SPARE_MOVE_ID] as usize;
    if spare == 0 {
        bail!("spare move id {SPARE_MOVE_ID:#x} owns no record");
    }
    let record_at = |i: usize| MOVE_POWER_TABLE + i * MOVE_POWER_STRIDE;
    let mut record = overlay
        .get(record_at(map[spell] as usize)..record_at(map[spell] as usize + 1))
        .context("battle overlay shorter than its move-power table")?
        .to_vec();
    record[..2].fill(0);
    patcher.patch_prot_entry(BATTLE_OVERLAY_ENTRY, record_at(spare) as u64, &record)?;
    if map_swing_byte {
        patcher.patch_prot_entry(
            BATTLE_OVERLAY_ENTRY,
            (MOVE_POWER_MAP + FIREBALL_MOVE_ID) as u64,
            &[spare as u8],
        )?;
    }
    report.notes.push(format!(
        "fireball -> move-power record {spare}: copy of record {} (monster magic {spell:#x}) with no damage power",
        map[spell]
    ));
    Ok(if map_swing_byte {
        FIREBALL_MOVE_ID as u8
    } else {
        SPARE_MOVE_ID as u8
    })
}
