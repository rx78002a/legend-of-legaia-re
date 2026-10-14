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
    /// fireball - instead of a melee move (stage 1: the art walks up first).
    /// `None` keeps melee in every art.
    pub fireball_art: Option<String>,
    /// Direction swing `0xF` plays the boss's cast: the fireball's clip lives
    /// in that swing record and the move-power map points its queue byte at
    /// the boss's spell record.
    pub fireball_swing: bool,
    /// Stage 2 hooks ([`direct_cast`]): the arts named in `cast_arts` collapse
    /// to the fireball swing and play in place; a plain Up press plays the
    /// Down swing so the fireball stays a technique.
    pub direct_cast: bool,
    /// Arts (by inline name) that cast the fireball when `direct_cast` is on.
    pub cast_arts: Vec<String>,
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
            fireball_art: None,
            fireball_swing: true,
            direct_cast: true,
            cast_arts: vec!["Somersault".to_string()],
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
    if opts.fireball_art.is_some() || opts.fireball_swing {
        route_fireball_move(patcher, opts.monster, opts.fireball_damage, &mut report)?;
    }
    if opts.direct_cast {
        if !opts.fireball_swing {
            bail!("direct cast collapses arts onto the fireball swing: it needs fireball_swing");
        }
        let mut constants = Vec::new();
        for name in &opts.cast_arts {
            let art = plan
                .arts
                .iter()
                .find(|a| !a.base && a.name.eq_ignore_ascii_case(name))
                .with_context(|| format!("no art record named {name:?}"))?;
            constants.push(direct_cast::TABLE_BASE + art.index as u8);
        }
        let fresh = direct_cast::install(patcher, &constants)?;
        report.notes.push(format!(
            "direct cast hooks {}: art constants {constants:x?} collapse to the fireball swing",
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

/// Give the fireball art's move id a move-power record of its own: a copy of
/// the boss's special-attack record (launch and contact effects, homing, cue)
/// with the damage power zeroed. The clip's terminator installs it and the
/// projectile spawns, while the art's own hit events do the damage; a kill by
/// the projectile's damage path skips the victim's reaction pose and the
/// enemy vanishes instead of collapsing.
fn route_fireball_move(
    patcher: &mut DiscPatcher,
    monster: u16,
    keep_damage: bool,
    report: &mut Report,
) -> Result<()> {
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
    if map[FIREBALL_MOVE_ID] != 0 {
        bail!("move id {FIREBALL_MOVE_ID:#x} is already mapped to a power record");
    }
    if keep_damage {
        let id = map[spell];
        patcher.patch_prot_entry(
            BATTLE_OVERLAY_ENTRY,
            (MOVE_POWER_MAP + FIREBALL_MOVE_ID) as u64,
            &[id],
        )?;
        report.notes.push(format!(
            "move id {FIREBALL_MOVE_ID:#x} -> move-power record {id} (monster magic {spell:#x})"
        ));
        return Ok(());
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
    patcher.patch_prot_entry(
        BATTLE_OVERLAY_ENTRY,
        (MOVE_POWER_MAP + FIREBALL_MOVE_ID) as u64,
        &[spare as u8],
    )?;
    report.notes.push(format!(
        "move id {FIREBALL_MOVE_ID:#x} -> move-power record {spare}: copy of record {} (monster magic {spell:#x}) with no damage power",
        map[spell]
    ));
    Ok(())
}
