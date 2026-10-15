//! Disc-gated: a boss's voice lands in the host's program and nowhere else.

use legaia_boss_swap::voice;
use legaia_boss_swap::{Options, apply};
use legaia_disc_patch::disc::DiscPatcher;
use legaia_party_swap::playerize::boss;

fn disc() -> Option<Vec<u8>> {
    let Some(path) = std::env::var_os("LEGAIA_DISC_BIN") else {
        eprintln!("LEGAIA_DISC_BIN unset: skipping");
        return None;
    };
    Some(std::fs::read(path).expect("read disc"))
}

#[test]
fn songi_voice_replaces_vahns_program_only() {
    let Some(image) = disc() else { return };
    let retail = DiscPatcher::open(image.clone()).unwrap();
    let before = retail.read_entry(869).unwrap();
    let mut p = DiscPatcher::open(image).unwrap();
    let report = apply(
        &mut p,
        &Options {
            monster: 76,
            ..Options::default()
        },
    )
    .unwrap();
    assert!(
        report
            .notes
            .iter()
            .any(|n| n.starts_with("voice cue 0x17b")),
        "{:#?}",
        report.notes
    );
    let after = p.read_entry(869).unwrap();
    assert_ne!(before, after);

    // every changed byte lies in a program-7 tone's attributes or body
    let off = legaia_vab::find_vabs(&before)[0];
    let r = legaia_vab::parse(&before, off).unwrap();
    let pages: Vec<usize> = r
        .programs
        .iter()
        .enumerate()
        .filter(|(_, p)| p.tones > 0)
        .map(|(i, _)| i)
        .collect();
    let page = pages.iter().position(|&p| p == 7).unwrap();
    let mut allowed: Vec<(usize, usize)> = Vec::new();
    for t in 0..r.programs[7].tones as usize {
        let o = r.header_offset
            + legaia_vab::VAB_HEADER_SIZE
            + legaia_vab::PROGRAMS_TABLE_SIZE
            + (page * legaia_vab::TONES_PER_PROGRAM + t) * legaia_vab::TONE_SIZE;
        allowed.push((o, o + 20));
        let vag = i16::from_le_bytes(before[o + 22..o + 24].try_into().unwrap()) as usize;
        let (so, sn) = (
            r.vag_samples[vag - 1].byte_offset,
            r.vag_samples[vag - 1].size,
        );
        allowed.push((so + 4, so + 4 + sn));
    }
    let stray: Vec<usize> = (0..before.len())
        .filter(|&i| before[i] != after[i])
        .filter(|&i| !allowed.iter().any(|&(a, b)| (a..b).contains(&i)))
        .collect();
    assert!(
        stray.is_empty(),
        "bytes changed outside program 7: {:#x?}",
        &stray[..stray.len().min(8)]
    );

    // the swing grunt reaches the host through a cue that now plays Songi's
    let archive = retail.read_entry_footprint(867).unwrap();
    let mut clips = boss::boss_clips_raw(&archive, 76).unwrap();
    let plan = voice::plan(&retail, 76, &mut clips).unwrap().unwrap();
    let swing = plan.moves.iter().find(|m| m.from == 0x17B).unwrap();
    assert!(swing.to < 0xC8 && swing.bytes.0 == swing.bytes.1);
    // no foreign id is left in any clip's cue track
    for c in &clips {
        for o in (0x54..0x74).step_by(4) {
            let cue = u16::from_le_bytes([c.head[o + 2], c.head[o + 3]]);
            assert!(
                cue < 0x64 || boss::HOST_VOICE_CUES.contains(&cue),
                "{cue:#x}"
            );
        }
    }
}

#[test]
fn a_monster_without_voice_cues_leaves_the_bank_alone() {
    let Some(image) = disc() else { return };
    let retail = DiscPatcher::open(image.clone()).unwrap();
    let archive = retail.read_entry_footprint(867).unwrap();
    // strip every cue of a clip set: nothing to plan
    let mut clips = boss::boss_clips_raw(&archive, 76).unwrap();
    for c in &mut clips {
        c.head[0x54..0x74].fill(0);
    }
    assert!(voice::plan(&retail, 76, &mut clips).unwrap().is_none());
}
