//! Disc-gated: the stage-2 hooks land on the real battle overlay and SCUS,
//! survive the PPF round trip, and installing them twice is a no-op.

use legaia_boss_swap::direct_cast;
use legaia_boss_swap::{Options, apply};
use legaia_code_hooks::arenas::SCUS_GAP_VA;
use legaia_code_hooks::mips as m;
use legaia_disc_patch::disc::DiscPatcher;

fn disc() -> Option<Vec<u8>> {
    let Some(path) = std::env::var_os("LEGAIA_DISC_BIN") else {
        eprintln!("LEGAIA_DISC_BIN unset: skipping");
        return None;
    };
    Some(std::fs::read(path).expect("read disc"))
}

/// Check the hook words and stubs on a patched image.
fn check(p: &DiscPatcher) {
    let overlay = p.read_entry(898).expect("PROT 898");
    let at = (0x801E_31F4u32 - 0x801C_E818) as usize;
    let word = u32::from_le_bytes(overlay[at..at + 4].try_into().unwrap());
    assert_eq!(word, m::jal(SCUS_GAP_VA), "range call");

    // Somersault is bank row 23: constant 0x27.
    let scus = p.read_named_file("SCUS_942.54").expect("SCUS");
    let off = |va: u32| legaia_game_tables::item_names::file_offset_for_va(&scus, va).unwrap();
    let begin = direct_cast::assemble_begin(direct_cast::table_va());
    assert_eq!(&scus[off(SCUS_GAP_VA)..][..begin.len()], begin.as_slice());
    let table_at = off(direct_cast::table_va());
    assert_eq!(scus[table_at + 0x27 - 0x10], 1, "Somersault is a cast art");
    assert_eq!(scus[table_at + 0x1F - 0x10], 0, "Cyclone is not");
    let snap = direct_cast::assemble_snap();
    let snap_va = direct_cast::snap_va();
    assert_eq!(&scus[off(snap_va)..][..snap.len()], snap.as_slice());
    let site = off(0x8004_BFA8);
    assert_eq!(
        u32::from_le_bytes(scus[site..site + 4].try_into().unwrap()),
        m::jal(snap_va),
        "commit-time range call"
    );
}

#[test]
fn hook_lands_on_the_patched_image() {
    let Some(image) = disc() else { return };
    let mut p = DiscPatcher::open(image).expect("open disc");
    apply(&mut p, &Options::default()).expect("apply boss swap");
    check(&p);
    assert!(
        !direct_cast::install(&mut p, &[0x27]).unwrap(),
        "second install is a no-op"
    );
}

/// The PPF a user applies carries the hooks: diff, serialise, apply onto the
/// untouched image and read the words back.
#[test]
fn hooks_survive_the_ppf_round_trip() {
    let Some(original) = disc() else { return };
    let mut p = DiscPatcher::open(original.clone()).expect("open disc");
    apply(&mut p, &Options::default()).expect("apply boss swap");
    let patched = p.into_image();
    let runs = legaia_disc_patch::ppf::diff_runs(&original, &patched);
    let ppf = legaia_disc_patch::ppf::write_ppf3("test", &runs);
    let mut applied = original.clone();
    legaia_disc_patch::ppf::apply_ppf3(&mut applied, &ppf).expect("apply ppf");
    assert!(applied == patched, "PPF reproduces the patched image");
    check(&DiscPatcher::open(applied).expect("open patched"));
}
