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
