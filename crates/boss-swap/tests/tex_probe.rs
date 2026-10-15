//! Ignored probe: per-primitive palette / page / UV box of a monster's TMD.
use legaia_actor_formats::monster_archive;
use legaia_disc_patch::disc::DiscPatcher;
use legaia_tmd::legaia_prims;

#[test]
#[ignore]
fn dump_prims() {
    let Some(path) = std::env::var_os("LEGAIA_DISC_BIN") else {
        return;
    };
    let id: u16 = std::env::var("PROBE_ID").unwrap().parse().unwrap();
    let p = DiscPatcher::open(std::fs::read(path).unwrap()).unwrap();
    let archive = p.read_entry_footprint(867).unwrap();
    let mesh = monster_archive::mesh(&archive, id).unwrap().unwrap();
    let bytes = mesh.tmd_bytes();
    let tmd = legaia_tmd::parse(bytes).unwrap();
    for (oi, o) in tmd.objects.iter().enumerate() {
        for g in legaia_prims::iter_groups_lenient(
            bytes,
            o.primitives_byte_offset,
            o.primitives_byte_size,
        ) {
            for (pi, p) in g.prims.iter().enumerate() {
                if p.uvs.is_empty() {
                    continue;
                }
                let (u0, u1) = (
                    p.uvs.iter().map(|u| u.0).min().unwrap(),
                    p.uvs.iter().map(|u| u.0).max().unwrap(),
                );
                let (v0, v1) = (
                    p.uvs.iter().map(|u| u.1).min().unwrap(),
                    p.uvs.iter().map(|u| u.1).max().unwrap(),
                );
                println!(
                    "obj {oi:2} flags {:#06x} prim {pi:3} pal {:2} cba {:#06x} tsb {:#06x} uv u {u0:3}..{u1:3} v {v0:3}..{v1:3}",
                    g.header.flags,
                    p.cba & 0x3F,
                    p.cba,
                    p.tsb
                );
            }
        }
    }
}

#[test]
#[ignore]
fn scan_foreign() {
    let Some(path) = std::env::var_os("LEGAIA_DISC_BIN") else {
        return;
    };
    let p = DiscPatcher::open(std::fs::read(path).unwrap()).unwrap();
    let archive = p.read_entry_footprint(867).unwrap();
    for id in 1u16..=194 {
        let Ok(Some(mesh)) = monster_archive::mesh(&archive, id) else {
            continue;
        };
        let bytes = mesh.tmd_bytes();
        let Ok(tmd) = legaia_tmd::parse(bytes) else {
            continue;
        };
        let mut total = 0;
        let mut foreign = 0;
        let mut kinds = std::collections::BTreeSet::new();
        for o in &tmd.objects {
            for g in legaia_prims::iter_groups_lenient(
                bytes,
                o.primitives_byte_offset,
                o.primitives_byte_size,
            ) {
                for p in &g.prims {
                    if p.uvs.is_empty() {
                        continue;
                    }
                    total += 1;
                    if (p.cba >> 6) != 481 || (p.tsb & 0xF) != 5 {
                        foreign += 1;
                        kinds.insert((p.cba >> 6, p.tsb & 0xFF));
                    }
                }
            }
        }
        if foreign > 0 {
            println!("SCAN id {id} prims {total} foreign {foreign} kinds {kinds:?}");
        }
    }
}

#[test]
#[ignore]
fn untextured_counts() {
    let Some(path) = std::env::var_os("LEGAIA_DISC_BIN") else {
        return;
    };
    let id: u16 = std::env::var("PROBE_ID").unwrap().parse().unwrap();
    let p = DiscPatcher::open(std::fs::read(path).unwrap()).unwrap();
    let archive = p.read_entry_footprint(867).unwrap();
    let mesh = monster_archive::mesh(&archive, id).unwrap().unwrap();
    let bytes = mesh.tmd_bytes();
    let tmd = legaia_tmd::parse(bytes).unwrap();
    for (oi, o) in tmd.objects.iter().enumerate() {
        let (mut tex, mut flat) = (0, 0);
        let mut cols = std::collections::BTreeSet::new();
        for g in legaia_prims::iter_groups_lenient(
            bytes,
            o.primitives_byte_offset,
            o.primitives_byte_size,
        ) {
            for p in &g.prims {
                if p.uvs.is_empty() {
                    flat += 1;
                    for c in &p.colors {
                        cols.insert(*c);
                    }
                } else {
                    tex += 1;
                }
            }
        }
        println!(
            "UNTEX obj {oi} textured {tex} flat {flat} colours {:?}",
            cols.iter().take(4).collect::<Vec<_>>()
        );
    }
}

#[test]
#[ignore]
fn part_counts() {
    let Some(path) = std::env::var_os("LEGAIA_DISC_BIN") else {
        return;
    };
    let p = DiscPatcher::open(std::fs::read(path).unwrap()).unwrap();
    let archive = p.read_entry_footprint(867).unwrap();
    let mut by: std::collections::BTreeMap<usize, Vec<u16>> = Default::default();
    for id in 1u16..=194 {
        let Ok(Some(mesh)) = monster_archive::mesh(&archive, id) else {
            continue;
        };
        if let Ok(tmd) = legaia_tmd::parse(mesh.tmd_bytes()) {
            by.entry(tmd.objects.len()).or_default().push(id);
        }
    }
    for (n, ids) in by {
        println!("{n:3} parts: {} monsters {:?}", ids.len(), ids);
    }
}

#[test]
#[ignore]
fn flat_prims() {
    let Some(path) = std::env::var_os("LEGAIA_DISC_BIN") else {
        return;
    };
    let id: u16 = std::env::var("PROBE_ID").unwrap().parse().unwrap();
    let p = DiscPatcher::open(std::fs::read(path).unwrap()).unwrap();
    let archive = p.read_entry_footprint(867).unwrap();
    let mesh = monster_archive::mesh(&archive, id).unwrap().unwrap();
    let bytes = mesh.tmd_bytes();
    let tmd = legaia_tmd::parse(bytes).unwrap();
    for (oi, o) in tmd.objects.iter().enumerate() {
        for g in legaia_prims::iter_groups_lenient(
            bytes,
            o.primitives_byte_offset,
            o.primitives_byte_size,
        ) {
            for (pi, p) in g.prims.iter().enumerate() {
                if p.uvs.is_empty() {
                    println!(
                        "FLAT obj {oi} flags {:#06x} prim {pi} colors {:?}",
                        g.header.flags, p.colors
                    );
                }
            }
        }
    }
}

#[test]
#[ignore]
fn gap_bytes() {
    let Some(path) = std::env::var_os("LEGAIA_DISC_BIN") else {
        return;
    };
    let p = DiscPatcher::open(std::fs::read(path).unwrap()).unwrap();
    let scus = p.read_named_file("SCUS_942.54").unwrap();
    for (lo, hi) in [(0x8007_AB00u32, 0x8007_AB80u32), (0x8007_AE00, 0x8007_AF40)] {
        let o = legaia_game_tables::item_names::file_offset_for_va(&scus, lo).unwrap();
        let n = (hi - lo) as usize;
        println!(
            "GAP {lo:#x}..{hi:#x} nonzero {:?}",
            scus[o..o + n]
                .iter()
                .enumerate()
                .filter(|(_, b)| **b != 0)
                .map(|(i, _)| i)
                .take(8)
                .collect::<Vec<_>>()
        );
    }
}

#[test]
#[ignore]
fn scus_words() {
    let Some(path) = std::env::var_os("LEGAIA_DISC_BIN") else {
        return;
    };
    let lo = u32::from_str_radix(&std::env::var("PROBE_LO").unwrap(), 16).unwrap();
    let hi = u32::from_str_radix(&std::env::var("PROBE_HI").unwrap(), 16).unwrap();
    let p = DiscPatcher::open(std::fs::read(path).unwrap()).unwrap();
    let scus = p.read_named_file("SCUS_942.54").unwrap();
    let o = legaia_game_tables::item_names::file_offset_for_va(&scus, lo).unwrap();
    for i in 0..((hi - lo) / 4) as usize {
        let w = u32::from_le_bytes(scus[o + i * 4..o + i * 4 + 4].try_into().unwrap());
        println!("{:08x}: {w:08x}", lo + i as u32 * 4);
    }
}

#[test]
#[ignore]
fn record_spells() {
    let Some(path) = std::env::var_os("LEGAIA_DISC_BIN") else {
        return;
    };
    let p = DiscPatcher::open(std::fs::read(path).unwrap()).unwrap();
    let scus = p.read_named_file("SCUS_942.54").unwrap();
    let archive = p.read_entry_footprint(867).unwrap();
    let table = legaia_game_tables::spell_names::SpellNameTable::from_scus(&scus).unwrap();
    for id in std::env::var("PROBE_IDS").unwrap().split(',') {
        let id: u16 = id.parse().unwrap();
        let block = monster_archive::decode_block(&archive, id)
            .unwrap()
            .unwrap();
        let spells = &block[0x21..0x24];
        let off = legaia_game_tables::item_names::file_offset_for_va(&scus, 0x8007_54C8).unwrap();
        let described: Vec<String> = spells
            .iter()
            .map(|&s| {
                let class = scus[off + s as usize * 12] as char;
                format!("{s:#x}({class}) {:?}", table.name(s))
            })
            .collect();
        println!("monster {id}: {described:?}");
    }
}
