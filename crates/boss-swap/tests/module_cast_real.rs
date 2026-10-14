//! Disc-gated: the cast-module route lands on the real modules and SCUS,
//! survives the PPF round trip and installs idempotently.

use legaia_boss_swap::module_cast::{self, Special};
use legaia_boss_swap::{Options, apply};
use legaia_code_hooks::arenas::SCUS_GAP_VA;
use legaia_code_hooks::mips as m;
use legaia_disc_patch::disc::DiscPatcher;
use legaia_party_swap::playerize::boss::Record0;

const SONGI_2ND: u16 = 136;

fn disc() -> Option<Vec<u8>> {
    let Some(path) = std::env::var_os("LEGAIA_DISC_BIN") else {
        eprintln!("LEGAIA_DISC_BIN unset: skipping");
        return None;
    };
    Some(std::fs::read(path).expect("read disc"))
}

fn opts() -> Options {
    Options {
        monster: SONGI_2ND,
        ..Options::default()
    }
}

fn word(entry: &[u8], va: u32) -> u32 {
    let at = (va - 0x801F_69D8) as usize;
    u32::from_le_bytes(entry[at..at + 4].try_into().unwrap())
}

fn check(p: &DiscPatcher) {
    let l = module_cast::layout();
    // applier call redirected to the route stub
    let overlay = p.read_entry(898).unwrap();
    let at = (0x801E_F9ACu32 - 0x801C_E818) as usize;
    assert_eq!(
        u32::from_le_bytes(overlay[at..at + 4].try_into().unwrap()),
        m::jal(SCUS_GAP_VA)
    );
    // stub, table and helpers in SCUS
    let scus = p.read_named_file("SCUS_942.54").unwrap();
    let off = |va: u32| legaia_game_tables::item_names::file_offset_for_va(&scus, va).unwrap();
    let route = module_cast::assemble_route(l.table, l.credit);
    assert_eq!(&scus[off(SCUS_GAP_VA)..][..route.len()], route.as_slice());
    assert_eq!(scus[off(l.table) + 0x27 - 0x10], 0x4B);
    assert_eq!(scus[off(l.table) + 0x29 - 0x10], 0x4C);
    let end = module_cast::assemble_end();
    assert_eq!(&scus[off(l.end)..][..end.len()], end.as_slice());
    // module words
    let m936 = p.read_entry(936).unwrap();
    for e in module_cast::edits_936(&l) {
        assert_eq!(word(&m936, e.va), e.replace, "936 {:#x}", e.va);
    }
    let m937 = p.read_entry(937).unwrap();
    for e in module_cast::edits_937() {
        assert_eq!(word(&m937, e.va), e.replace, "937 {:#x}", e.va);
    }
    // stage rows authored in record[0]
    let r0 = Record0::parse(&p.read_entry_footprint(863).unwrap()).unwrap();
    for row in [0x0A, 0x06] {
        let (parts, frames) = r0.stream_shape(row).expect("row");
        assert!(parts > 0 && frames > 0, "row {row:#x} authored");
        let at = r0.entry_offset(row).unwrap();
        assert_eq!(r0.decoded[at + 0x77], 0x0A, "attach key");
        assert_eq!(r0.decoded[at + 0x84], 0xFF, "hold window");
    }
}

#[test]
fn route_lands_on_the_patched_image() {
    let Some(image) = disc() else { return };
    let mut p = DiscPatcher::open(image).expect("open disc");
    apply(&mut p, &opts()).expect("apply boss swap");
    check(&p);
    let specials = [
        Special {
            constant: 0x27,
            spell: 0x4B,
        },
        Special {
            constant: 0x29,
            spell: 0x4C,
        },
    ];
    assert!(
        !module_cast::install(&mut p, &specials).unwrap(),
        "second install is a no-op"
    );
}

#[test]
fn route_survives_the_ppf_round_trip() {
    let Some(original) = disc() else { return };
    let mut p = DiscPatcher::open(original.clone()).expect("open disc");
    apply(&mut p, &opts()).expect("apply boss swap");
    let patched = p.into_image();
    let runs = legaia_disc_patch::ppf::diff_runs(&original, &patched);
    let ppf = legaia_disc_patch::ppf::write_ppf3("test", &runs);
    let mut applied = original.clone();
    legaia_disc_patch::ppf::apply_ppf3(&mut applied, &ppf).expect("apply ppf");
    assert!(applied == patched);
    check(&DiscPatcher::open(applied).unwrap());
}

/// Songi 1st keeps the fireball route and gets no module edits.
#[test]
fn songi_first_form_is_unchanged() {
    let Some(image) = disc() else { return };
    let mut p = DiscPatcher::open(image.clone()).expect("open disc");
    apply(&mut p, &Options::default()).expect("apply boss swap");
    assert_eq!(
        p.read_entry(936).unwrap(),
        DiscPatcher::open(image).unwrap().read_entry(936).unwrap()
    );
}
