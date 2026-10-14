//! Play as a boss: a party slot's **battle** model, motions and (later) stats
//! and voices come from a monster of the archive. Field models are untouched.
//!
//! The kernels live in `legaia_party_swap::playerize::boss`; this crate is the
//! disc driver on top of them.

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
    /// Every swing and art plays the boss's cast (its fireball), not the
    /// boss's melee moves.
    pub fireball: bool,
    /// Mute the host character's own voice banks (arts shout, fanfares,
    /// swing grunt, victory barks).
    pub mute_host_voice: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            monster: 76,
            scale: 0.8,
            fireball: true,
            mute_host_voice: true,
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
    let plan = if opts.fireball {
        let map = boss::fireball_map(&mut clips)?;
        boss::plan_boss_with(&rec0, map, main_n, base_n)?
    } else {
        boss::plan_boss(&rec0, &clips, main_n, base_n)?
    };
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
