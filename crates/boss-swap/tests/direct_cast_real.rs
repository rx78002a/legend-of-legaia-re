//! Disc-gated: the stage-2 hook lands on the real battle overlay and SCUS,
//! and installing it twice is a no-op.

use legaia_boss_swap::direct_cast;
use legaia_boss_swap::{Options, apply};
use legaia_code_hooks::arenas::{ARENA1_VA, SCUS_GAP_VA};
use legaia_code_hooks::mips as m;
use legaia_disc_patch::disc::DiscPatcher;

#[test]
fn hook_lands_on_the_patched_image() {
    let Some(path) = std::env::var_os("LEGAIA_DISC_BIN") else {
        eprintln!("LEGAIA_DISC_BIN unset: skipping");
        return;
    };
    let image = std::fs::read(path).expect("read disc");
    let mut p = DiscPatcher::open(image).expect("open disc");
    apply(&mut p, &Options::default()).expect("apply boss swap");

    let overlay = p.read_entry(898).expect("PROT 898");
    let word = |va: u32| {
        let at = (va - 0x801C_E818) as usize;
        u32::from_le_bytes(overlay[at..at + 4].try_into().unwrap())
    };
    assert_eq!(word(0x801E_31F4), m::jal(SCUS_GAP_VA), "range call");
    assert_eq!(word(0x801D_F250), m::jal(ARENA1_VA), "projectile fetch");
    assert_eq!(word(0x801E_187C), m::jal(ARENA1_VA), "damage fetch");

    // Somersault is bank row 23: constant 0x27, casting the boss's spell.
    let casts = [direct_cast::CastArt {
        constant: 0x27,
        spell: 0x2A,
    }];
    let scus = p.read_named_file("SCUS_942.54").expect("SCUS");
    let at = |va: u32| legaia_game_tables::item_names::file_offset_for_va(&scus, va).unwrap();
    let (begin, table_va) = direct_cast::assemble_begin(&casts);
    assert_eq!(&scus[at(SCUS_GAP_VA)..][..begin.len()], begin.as_slice());
    let fetch = direct_cast::assemble_fetch(table_va);
    assert_eq!(&scus[at(ARENA1_VA)..][..fetch.len()], fetch.as_slice());
    let snap = direct_cast::assemble_snap(table_va);
    let snap_va = direct_cast::snap_va();
    assert_eq!(&scus[at(snap_va)..][..snap.len()], snap.as_slice());
    let site = at(0x8004_BFA8);
    assert_eq!(
        u32::from_le_bytes(scus[site..site + 4].try_into().unwrap()),
        m::jal(snap_va),
        "commit-time range call"
    );

    assert!(
        !direct_cast::install(&mut p, &casts).unwrap(),
        "second install is a no-op"
    );
}

/// The PPF a user applies carries the hooks: diff, serialise, apply onto the
/// untouched image and read the words back.
#[test]
fn hooks_survive_the_ppf_round_trip() {
    let Some(path) = std::env::var_os("LEGAIA_DISC_BIN") else {
        eprintln!("LEGAIA_DISC_BIN unset: skipping");
        return;
    };
    let original = std::fs::read(path).expect("read disc");
    let mut p = DiscPatcher::open(original.clone()).expect("open disc");
    apply(&mut p, &Options::default()).expect("apply boss swap");
    let patched = p.into_image();
    let runs = legaia_disc_patch::ppf::diff_runs(&original, &patched);
    let ppf = legaia_disc_patch::ppf::write_ppf3("test", &runs);
    let mut applied = original.clone();
    legaia_disc_patch::ppf::apply_ppf3(&mut applied, &ppf).expect("apply ppf");
    assert!(applied == patched, "PPF reproduces the patched image");

    let q = DiscPatcher::open(applied).expect("open patched");
    let overlay = q.read_entry(898).expect("PROT 898");
    let at = (0x801E_31F4u32 - 0x801C_E818) as usize;
    let word = u32::from_le_bytes(overlay[at..at + 4].try_into().unwrap());
    assert_eq!(word, m::jal(SCUS_GAP_VA));
}
