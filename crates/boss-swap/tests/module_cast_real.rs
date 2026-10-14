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
    // applier call redirected to the route stub, debit to the prep helper
    let overlay = p.read_entry(898).unwrap();
    let ov = |va: u32| {
        let at = (va - 0x801C_E818) as usize;
        u32::from_le_bytes(overlay[at..at + 4].try_into().unwrap())
    };
    assert_eq!(ov(0x801E_F9AC), m::jal(SCUS_GAP_VA));
    assert_eq!(ov(0x801E_4584), m::jal(l.prep));
    // stub, helper and tables in SCUS
    let scus = p.read_named_file("SCUS_942.54").unwrap();
    let off = |va: u32| legaia_game_tables::item_names::file_offset_for_va(&scus, va).unwrap();
    let route = module_cast::assemble_route(l.table, None);
    assert_eq!(&scus[off(SCUS_GAP_VA)..][..route.len()], route.as_slice());
    let prep = module_cast::assemble_prep(&l);
    assert_eq!(&scus[off(l.prep)..][..prep.len()], prep.as_slice());
    assert_eq!(scus[off(l.table) + 0x27 - 0x10], 0x4B);
    assert_eq!(scus[off(l.table) + 0x29 - 0x10], 0x4C);
    // module words
    let m936 = p.read_entry(936).unwrap();
    for e in module_cast::edits_936() {
        assert_eq!(word(&m936, e.va), e.replace, "936 {:#x}", e.va);
    }
    let m937 = p.read_entry(937).unwrap();
    for e in module_cast::edits_937() {
        assert_eq!(word(&m937, e.va), e.replace, "937 {:#x}", e.va);
    }
    // no edited word reads a register in the delay slot of the load before it
    for (img, edits) in [
        (&m936, module_cast::edits_936()),
        (&m937, module_cast::edits_937()),
    ] {
        let ws: Vec<u32> = img
            .chunks(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        for e in &edits {
            let i = ((e.va - 0x801F_69D8) / 4) as usize;
            let lo = i.saturating_sub(1);
            let hi = (i + 3).min(ws.len());
            for h in legaia_boss_swap::hazard::load_hazards(&ws[lo..hi]) {
                // the pair (lo + h, lo + h + 1) must not involve an edited word
                let at = lo + h;
                assert!(
                    !(at == i || at + 1 == i),
                    "load-delay hazard at {:#x}",
                    0x801F_69D8 + at as u32 * 4
                );
            }
        }
    }
    // stage rows authored in record[0], the pool entries behind the table
    let r0 = Record0::parse(&p.read_entry_footprint(863).unwrap()).unwrap();
    // row A holds the first stage pose; row 6 keeps its retail stub (the prep
    // helper repoints it per cast)
    let (parts, frames) = r0.stream_shape(0x0A).expect("row A");
    assert!(parts > 0 && frames > 0, "row 0xa authored");
    let at = r0.entry_offset(0x0A).unwrap();
    assert_eq!(r0.decoded[at + 0x77], 0x0A, "attach key");
    assert_eq!(r0.decoded[at + 0x84], 0xFF, "hold window");
    assert!(
        r0.stream_shape(0x06).is_none_or(|(p, f)| p == 0 || f == 0),
        "row 6 is not authored"
    );
    for (spell, slot) in [(0x4Bu8, 0usize), (0x4C, 1)] {
        let at = off(l.rows) + slot * 4;
        assert_eq!(scus[at], spell, "slot {slot} routes spell {spell:#x}");
        let entry = u16::from_le_bytes(scus[at + 2..at + 4].try_into().unwrap()) as usize;
        assert!(entry != 0, "spell {spell:#x} has a pool entry");
        assert!(entry + 0xAC + 2 < r0.decoded.len(), "inside record[0]");
        assert!(entry < r0.clut_a, "below the load-time scratch");
        let (parts, frames) = (
            r0.decoded[entry + 0xAC] as usize,
            r0.decoded[entry + 0xAD] as usize,
        );
        assert!(parts > 0 && frames > 0, "spell {spell:#x} pool stream");
        assert_eq!(r0.decoded[entry + 0x77], 0x0A);
        assert_eq!(r0.decoded[entry + 0x84], 0xFF);
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
    // the pool offsets the build chose, read back from the installed table
    let scus = p.read_named_file("SCUS_942.54").unwrap();
    let l = module_cast::layout();
    let at = legaia_game_tables::item_names::file_offset_for_va(&scus, l.rows).unwrap();
    let pool: Vec<(u8, usize)> = [0x4Bu8, 0x4C]
        .iter()
        .enumerate()
        .map(|(i, &sp)| {
            let off = u16::from_le_bytes(scus[at + i * 4 + 2..at + i * 4 + 4].try_into().unwrap());
            (sp, usize::from(off))
        })
        .collect();
    assert!(
        !module_cast::install(&mut p, &specials, &pool, 0).unwrap(),
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

/// Cort's first form (#180) casts only Guilty Cross (PROT 944): one routed
/// spell, one stage immediate, no pooled pose, and a melee set cloned from its
/// cast clips (it has no attack of its own).
#[test]
fn cort_first_form_routes_guilty_cross() {
    let Some(image) = disc() else { return };
    let mut p = DiscPatcher::open(image).expect("open disc");
    let report = apply(
        &mut p,
        &Options {
            monster: 180,
            ..Options::default()
        },
    )
    .expect("apply boss swap");
    assert!(
        report
            .notes
            .iter()
            .any(|n| n.contains("swing clips cloned"))
    );
    let m944 = p.read_entry(944).unwrap();
    for e in module_cast::edits_944() {
        assert_eq!(word(&m944, e.va), e.replace, "944 {:#x}", e.va);
    }
    let scus = p.read_named_file("SCUS_942.54").unwrap();
    let l = module_cast::layout();
    let off = |va: u32| legaia_game_tables::item_names::file_offset_for_va(&scus, va).unwrap();
    assert_eq!(scus[off(l.table) + 0x27 - 0x10], 0x37);
    let at = off(l.rows);
    assert_eq!(
        &scus[at..at + 4],
        &[0x37, 0, 0, 0],
        "routed, no pooled pose"
    );
    // Mystic Circle rides Hyper Elbow with its one pose in the pool
    assert_eq!(scus[off(l.table) + 0x29 - 0x10], 0xB7);
    assert_eq!(scus[at + 4], 0xB7, "slot 1 routes Mystic Circle");
    let pooled = u16::from_le_bytes(scus[at + 6..at + 8].try_into().unwrap()) as usize;
    let guard = module_cast::assemble_mc_guard();
    assert_eq!(
        &scus[off(l.guard_938)..][..guard.len()],
        guard.as_slice(),
        "voice guard"
    );
    assert!(pooled != 0, "Mystic Circle has a pooled pose");
    // a hit on a monster special requests Cort's readef group (3 * 18)
    assert_eq!(scus[off(l.readef)], 0x36);
    let route = module_cast::assemble_route(l.table, Some(l.hook));
    assert_eq!(&scus[off(SCUS_GAP_VA)..][..route.len()], route.as_slice());
    let hook = module_cast::assemble_readef_request(&l);
    assert_eq!(&scus[off(l.hook)..][..hook.len()], hook.as_slice());
    let m938 = p.read_entry(938).unwrap();
    let edits = module_cast::edits_938();
    for e in &edits {
        assert_eq!(word(&m938, e.va), e.replace, "938 {:#x}", e.va);
    }
    let ws: Vec<u32> = m938
        .chunks(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    for e in &edits {
        let i = ((e.va - 0x801F_69D8) / 4) as usize;
        let lo = i.saturating_sub(1);
        let hi = (i + 3).min(ws.len());
        for h in legaia_boss_swap::hazard::load_hazards(&ws[lo..hi]) {
            let at = lo + h;
            assert!(
                !(at == i || at + 1 == i),
                "load-delay hazard at {:#x}",
                0x801F_69D8 + at as u32 * 4
            );
        }
    }
    let r0 = Record0::parse(&p.read_entry_footprint(863).unwrap()).unwrap();
    let (parts, frames) = r0.stream_shape(0x0A).expect("row A");
    assert!(parts > 0 && frames > 0, "row 0xa authored");
    assert!(pooled < r0.clut_a, "pool entry below the load-time scratch");
    assert!(r0.decoded[pooled + 0xAC] > 0 && r0.decoded[pooled + 0xAD] > 0);
    // Mystic Circle holds its clip's arms-out frame 15; decimated by two the
    // park lands on kept frame 8 (original 16), not on frame 7 (original 14,
    // the pose before the arms open)
    assert_eq!(
        &r0.decoded[pooled + 0x84..pooled + 0x87],
        &[0xFF, 8, 9],
        "hold window"
    );
}

/// Cort's left face half lives in the companion TIM (slot id + 8): the build
/// adopts it instead of dropping the prims.
#[test]
fn cort_face_comes_from_the_companion_page() {
    let Some(image) = disc() else { return };
    let mut p = DiscPatcher::open(image).expect("open disc");
    let archive = p.read_entry_footprint(867).unwrap();
    let page = legaia_actor_formats::monster_archive::companion_page(&archive, 180)
        .unwrap()
        .expect("companion TIM of #180");
    assert_eq!((page.cluts.len(), page.width), (16, 256));
    assert!(page.cluts[1][1] != 0, "palette 1 is populated");
    let report = apply(
        &mut p,
        &Options {
            monster: 180,
            ..Options::default()
        },
    )
    .expect("apply boss swap");
    assert!(
        !report.notes.iter().any(|n| n.contains("were dropped")),
        "{:?}",
        report.notes
    );
}

/// Probe: write the patched cast modules of a Cort build to `PROBE_DIR`
/// (`cargo test -p legaia-boss-swap --test module_cast_real dump_patched -- --ignored`).
#[test]
#[ignore]
fn dump_patched_modules() {
    let Some(image) = disc() else { return };
    let Some(dir) = std::env::var_os("PROBE_DIR") else {
        return;
    };
    let mut p = DiscPatcher::open(image).expect("open disc");
    apply(
        &mut p,
        &Options {
            monster: 180,
            ..Options::default()
        },
    )
    .expect("apply boss swap");
    for n in [938usize, 940, 944, 966] {
        std::fs::write(
            std::path::Path::new(&dir).join(format!("p{n:04}.bin")),
            p.read_entry(n).unwrap(),
        )
        .unwrap();
    }
}

/// Cort's Mystic Shield: the module, finisher and per-frame pass patches land
/// on the retail words they expect.
#[test]
fn cort_first_form_routes_mystic_shield() {
    let Some(image) = disc() else { return };
    let mut p = DiscPatcher::open(image).expect("open disc");
    apply(
        &mut p,
        &Options {
            monster: 180,
            specials: vec![(0x27, 0x37), (0x26, 0xAC), (0x29, 0xB7)],
            ..Options::default()
        },
    )
    .expect("apply boss swap");
    let l = module_cast::layout();
    let scus = p.read_named_file("SCUS_942.54").unwrap();
    let off = |va: u32| legaia_game_tables::item_names::file_offset_for_va(&scus, va).unwrap();
    assert_eq!(scus[off(l.table) + 0x26 - 0x10], 0xAC, "Slash Kick");
    let routed: Vec<u8> = (0..4).map(|i| scus[off(l.rows) + i * 4]).collect();
    assert!(routed.contains(&0xAC) && routed.contains(&0xB7) && routed.contains(&0x37));
    let m940 = p.read_entry(940).unwrap();
    for e in module_cast::edits_940() {
        assert_eq!(word(&m940, e.va), e.replace, "940 {:#x}", e.va);
    }
    let ov = p.read_entry(898).unwrap();
    let w = |va: u32| {
        let at = (va - 0x801C_E818) as usize;
        u32::from_le_bytes(ov[at..at + 4].try_into().unwrap())
    };
    assert_eq!(w(0x801D_DB90), m::j(l.shield_finisher));
    let pass = word_scus(&scus, off(0x8004_D55C));
    assert_eq!(pass, m::jal(l.shield_pass));
    let code = module_cast::assemble_shield_pass(&l);
    assert_eq!(&scus[off(l.shield_pass)..][..code.len()], code.as_slice());
    let code = module_cast::assemble_shield_finisher(&l);
    assert_eq!(
        &scus[off(l.shield_finisher)..][..code.len()],
        code.as_slice()
    );
}

fn word_scus(scus: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(scus[off..off + 4].try_into().unwrap())
}
