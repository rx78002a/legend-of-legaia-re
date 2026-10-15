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
    let begin = direct_cast::assemble_begin(direct_cast::table_va(), direct_cast::UP_MELEE_BYTE);
    assert_eq!(&scus[off(SCUS_GAP_VA)..][..begin.len()], begin.as_slice());
    let table_at = off(direct_cast::table_va());
    assert_eq!(scus[table_at + 0x27 - 0x10], 1, "Somersault is a cast art");
    assert_eq!(scus[table_at + 0x1F - 0x10], 0, "Cyclone is not");
    let snap = direct_cast::assemble_snap();
    let snap_va = direct_cast::snap_va();
    assert_eq!(&scus[off(snap_va)..][..snap.len()], snap.as_slice());
    // the strike and recovery states frame the caster through the camera stub
    let camera = direct_cast::assemble_camera();
    assert_eq!(
        &scus[off(legaia_boss_swap::module_cast::HOOK_VA)..][..camera.len()],
        camera.as_slice()
    );
    for site in [0x801E_36E8u32, 0x801E_3AC8] {
        let at = (site - 0x801C_E818) as usize;
        assert_eq!(
            u32::from_le_bytes(overlay[at..at + 4].try_into().unwrap()),
            m::jal(legaia_boss_swap::module_cast::HOOK_VA),
            "camera call {site:#x}"
        );
    }
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
        !direct_cast::install(&mut p, &[0x27], direct_cast::UP_MELEE_BYTE).unwrap(),
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

/// Record[0] row `0x0A` carries the displaced Up melee: a real stream and a
/// non-zero power run, where retail has a header-only placeholder.
#[test]
fn up_melee_row_is_authored() {
    let Some(image) = disc() else { return };
    let mut p = DiscPatcher::open(image).expect("open disc");
    let retail = legaia_party_swap::playerize::boss::Record0::parse(
        &p.read_entry_footprint(legaia_boss_swap::VAHN_ENTRY)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        retail.stream_shape(0x0A),
        Some((0, 0)),
        "retail placeholder"
    );
    apply(&mut p, &Options::default()).expect("apply boss swap");
    let r0 = legaia_party_swap::playerize::boss::Record0::parse(
        &p.read_entry_footprint(legaia_boss_swap::VAHN_ENTRY)
            .unwrap(),
    )
    .unwrap();
    let (parts, frames) = r0.stream_shape(0x0A).expect("row 0x0A");
    assert!(parts > 0 && frames > 0, "row 0x0A has a stream");
    let at = r0.entry_offset(0x0A).expect("row 0x0A offset");
    assert!(
        r0.decoded[at..at + 4].iter().any(|&b| b != 0),
        "power run is set"
    );
    assert_ne!(r0.stream_shape(0x0A), r0.stream_shape(0x0B), "own clip");

    // Off, the row stays the placeholder and a plain Up is the Down swing.
    let mut q = DiscPatcher::open(disc().unwrap()).unwrap();
    let opts = Options {
        up_melee: false,
        ..Options::default()
    };
    apply(&mut q, &opts).expect("apply without up melee");
    let r0 = legaia_party_swap::playerize::boss::Record0::parse(
        &q.read_entry_footprint(legaia_boss_swap::VAHN_ENTRY)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(r0.stream_shape(0x0A), Some((0, 0)));
}
