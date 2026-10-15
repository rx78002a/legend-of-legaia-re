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

#[test]
#[ignore]
fn spell_by_name() {
    let Some(path) = std::env::var_os("LEGAIA_DISC_BIN") else {
        return;
    };
    let p = DiscPatcher::open(std::fs::read(path).unwrap()).unwrap();
    let scus = p.read_named_file("SCUS_942.54").unwrap();
    let table = legaia_game_tables::spell_names::SpellNameTable::from_scus(&scus).unwrap();
    let want = std::env::var("PROBE_NAME").unwrap().to_lowercase();
    let off = legaia_game_tables::item_names::file_offset_for_va(&scus, 0x8007_54C8).unwrap();
    for id in 0..=255u8 {
        if let Some(n) = table.name(id)
            && n.to_lowercase().contains(&want)
        {
            let rec = &scus[off + id as usize * 12..off + id as usize * 12 + 12];
            println!("spell {id:#x} {n:?} stats {rec:02x?}");
        }
    }
}

#[test]
#[ignore]
fn dump_entries() {
    let Some(path) = std::env::var_os("LEGAIA_DISC_BIN") else {
        return;
    };
    let out = std::env::var("PROBE_OUT").unwrap();
    let p = DiscPatcher::open(std::fs::read(path).unwrap()).unwrap();
    for id in std::env::var("PROBE_ENTRIES").unwrap().split(',') {
        let id: usize = id.parse().unwrap();
        std::fs::write(format!("{out}/e{id}.bin"), p.read_entry(id).unwrap()).unwrap();
    }
}

#[test]
#[ignore]
fn record_sizes() {
    let Some(path) = std::env::var_os("LEGAIA_DISC_BIN") else {
        return;
    };
    let p = DiscPatcher::open(std::fs::read(path).unwrap()).unwrap();
    let scus = p.read_named_file("SCUS_942.54").unwrap();
    let archive = p.read_entry_footprint(867).unwrap();
    let off = legaia_game_tables::item_names::file_offset_for_va(&scus, 0x8007_8870).unwrap();
    println!("DAT_80078870..: {:02x?}", &scus[off..off + 0x20]);
    for id in std::env::var("PROBE_IDS").unwrap().split(',') {
        let id: u16 = id.parse().unwrap();
        let b = monster_archive::decode_block(&archive, id)
            .unwrap()
            .unwrap();
        println!("monster {id}: size byte +0x1F = {:#x}", b[0x1F]);
    }
}

#[test]
#[ignore]
fn dump_scus() {
    let Some(path) = std::env::var_os("LEGAIA_DISC_BIN") else {
        return;
    };
    let p = DiscPatcher::open(std::fs::read(path).unwrap()).unwrap();
    let scus = p.read_named_file("SCUS_942.54").unwrap();
    std::fs::write(std::env::var("PROBE_OUT").unwrap(), scus).unwrap();
}

#[test]
#[ignore]
fn voice_cues() {
    use legaia_container_formats::{bse_bank, vab_multi_bank};
    let Some(path) = std::env::var_os("LEGAIA_DISC_BIN") else {
        return;
    };
    let p = DiscPatcher::open(std::fs::read(path).unwrap()).unwrap();
    let archive = p.read_entry_footprint(867).unwrap();
    let bse = p.read_entry(888).unwrap();
    let snd = p.read_entry_footprint(891).unwrap();
    for id in std::env::var("PROBE_IDS").unwrap().split(',') {
        let id: u16 = id.parse().unwrap();
        println!("== monster {id}");
        let clips = legaia_party_swap::playerize::boss::boss_clips_raw(&archive, id).unwrap();
        let bank = vab_multi_bank::bank_bytes(&snd, usize::from(id) - 1);
        let rep = bank.and_then(|b| legaia_vab::parse_in_stream(b, 0).ok());
        if let Some(r) = &rep {
            let progs: Vec<(usize, u8)> = r
                .programs
                .iter()
                .enumerate()
                .filter(|(_, p)| p.tones > 0)
                .map(|(i, p)| (i, p.tones))
                .collect();
            println!(
                "bank: {} programs {:?}, {} vags {:?}",
                progs.len(),
                progs,
                r.vag_samples.len(),
                r.vag_samples.iter().map(|v| v.size).collect::<Vec<_>>()
            );
        } else {
            println!("bank: none");
        }
        for c in &clips {
            let h = &c.head;
            let mut cues = Vec::new();
            for o in (0x54..0x74).step_by(4) {
                let f = u16::from_le_bytes([h[o], h[o + 1]]);
                let cue = u16::from_le_bytes([h[o + 2], h[o + 3]]);
                if cue != 0 {
                    cues.push((f, cue));
                }
            }
            println!(
                "clip tag {:#04x} frames {:3} cues {cues:x?}",
                c.tag,
                c.len()
            );
            for &(_, cue) in &cues {
                if cue >= 0x64 {
                    let ring = cue + 0x19C;
                    if let Some(r) = bse_bank::record_for_cue(&bse, ring) {
                        println!(
                            "   cue {cue:#x} -> ring {ring:#x} row p{} t{} l{} n{:#x} cat{}",
                            r.program, r.tone, r.level, r.flags, r.category
                        );
                    }
                }
            }
        }
    }
}

#[test]
#[ignore]
fn battle_bank() {
    let Some(path) = std::env::var_os("LEGAIA_DISC_BIN") else {
        return;
    };
    let p = DiscPatcher::open(std::fs::read(path).unwrap()).unwrap();
    let buf = p.read_entry(869).unwrap();
    let offs = legaia_vab::find_vabs(&buf);
    println!("entry 869: {} bytes, vabs at {offs:?}", buf.len());
    for off in offs {
        let r = legaia_vab::parse(&buf, off).unwrap();
        println!("VAB at {off:#x}: ps {} vs {}", r.header.ps, r.header.vs);
        for (i, pr) in r.programs.iter().enumerate().filter(|(_, p)| p.tones > 0) {
            println!("  program {i}: tones {}", pr.tones);
        }
        let mut page = 0;
        for (i, pr) in r.programs.iter().enumerate().filter(|(_, p)| p.tones > 0) {
            for (t, tone) in r.tones[page].iter().take(pr.tones as usize).enumerate() {
                println!(
                    "    p{i} t{t}: vag {} vol {} center {} adsr {:04x}/{:04x}",
                    tone.vag, tone.vol, tone.center, tone.adsr1, tone.adsr2
                );
            }
            page += 1;
        }
        println!(
            "  vags: {:?}",
            r.vag_samples.iter().map(|v| v.size).collect::<Vec<_>>()
        );
    }
}

#[test]
#[ignore]
fn bse_rows() {
    use legaia_container_formats::bse_bank;
    let Some(path) = std::env::var_os("LEGAIA_DISC_BIN") else {
        return;
    };
    let p = DiscPatcher::open(std::fs::read(path).unwrap()).unwrap();
    let bse = p.read_entry(888).unwrap();
    let scus = p.read_named_file("SCUS_942.54").unwrap();
    for (i, r) in bse_bank::records(&bse).iter().enumerate() {
        if (0x30..0x70).contains(&i) {
            println!(
                "bse row {i:#x} (party cue {:#x}) p{} t{} l{} n{:#x} cat{}",
                i + 0x64,
                r.program,
                r.tone,
                r.level,
                r.flags,
                r.category
            );
        }
    }
    let off = legaia_game_tables::item_names::file_offset_for_va(&scus, 0x8006_F198).unwrap();
    for id in 0..0x64usize {
        let r = &scus[off + id * 8..off + id * 8 + 8];
        println!(
            "static id {id:#x} (ring) p{} t{} l{} n{:#x} cat{}",
            r[0], r[1], r[2], r[3], r[4]
        );
    }
}
