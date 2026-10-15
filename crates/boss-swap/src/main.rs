use anyhow::{Context, Result, bail};
use legaia_boss_swap::{Options, apply};
use legaia_disc_patch::disc::DiscPatcher;
use legaia_disc_patch::ppf;

fn usage() -> ! {
    eprintln!(
        "usage: boss-swap build <disc.bin|.cue> <out.ppf> [--monster N] [--scale F] [--no-fireball] [--fireball-art NAME] [--no-direct-cast] [--no-fireball-swing] [--cast-art NAME] [--special CONST=SPELL] [--no-up-melee] [--keep-voices] [--keep-support-anims] [--free-fireball]\n       \
         (writes a PPF 3.0 patch; the disc image is never modified)"
    );
    std::process::exit(2);
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("monster") && args.len() >= 4 {
        return monster(&args);
    }
    if args.first().map(String::as_str) == Some("clips") && args.len() >= 3 {
        return clips(&args);
    }
    if args.first().map(String::as_str) == Some("render") && args.len() >= 3 {
        return render(&args);
    }
    if args.first().map(String::as_str) != Some("build") || args.len() < 3 {
        usage();
    }
    let disc = std::path::Path::new(&args[1]);
    let out = &args[2];
    let mut opts = Options::default();
    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--monster" => {
                opts.monster = args.get(i + 1).context("--monster N")?.parse()?;
                i += 2;
            }
            "--scale" => {
                opts.scale = args.get(i + 1).context("--scale F")?.parse()?;
                i += 2;
            }
            "--attack-pause" => {
                opts.attack_pause = args.get(i + 1).context("--attack-pause N")?.parse()?;
                i += 2;
            }
            "--protect-part" => {
                opts.protect_parts
                    .push(args.get(i + 1).context("--protect-part N")?.parse()?);
                i += 2;
            }
            "--no-voices" => {
                opts.voices = false;
                i += 1;
            }
            "--reach" => {
                opts.reach = Some(args.get(i + 1).context("--reach N")?.parse()?);
                i += 2;
            }
            "--rate-div" => {
                opts.rate_div = args.get(i + 1).context("--rate-div N")?.parse()?;
                i += 2;
            }
            "--no-fireball" => {
                opts.fireball_art = None;
                opts.fireball_swing = false;
                opts.direct_cast = false;
                i += 1;
            }
            "--fireball-art" => {
                opts.fireball_art = Some(args.get(i + 1).context("--fireball-art NAME")?.clone());
                opts.fireball_swing = false;
                opts.direct_cast = false;
                i += 2;
            }
            "--no-direct-cast" => {
                opts.direct_cast = false;
                i += 1;
            }
            "--no-fireball-swing" => {
                opts.fireball_swing = false;
                opts.direct_cast = false;
                i += 1;
            }
            "--no-up-melee" => {
                opts.up_melee = false;
                i += 1;
            }
            "--special" => {
                let v = args.get(i + 1).context("--special CONST=SPELL (hex)")?;
                let (c, s) = v.split_once('=').context("--special CONST=SPELL (hex)")?;
                opts.specials.push((
                    u8::from_str_radix(c.trim_start_matches("0x"), 16)?,
                    u8::from_str_radix(s.trim_start_matches("0x"), 16)?,
                ));
                i += 2;
            }
            "--cast-art" => {
                opts.cast_arts
                    .push(args.get(i + 1).context("--cast-art NAME")?.clone());
                i += 2;
            }
            "--keep-voices" => {
                opts.mute_host_voice = false;
                i += 1;
            }
            "--keep-support-anims" => {
                opts.calm_support = false;
                i += 1;
            }
            "--free-fireball" => {
                opts.fireball_damage = false;
                i += 1;
            }
            other => bail!("unknown argument {other}"),
        }
    }
    let resolved = legaia_iso::raw::resolve_disc_path(disc)?;
    let original =
        std::fs::read(&resolved).with_context(|| format!("read {}", resolved.display()))?;
    let mut patcher = DiscPatcher::open(original.clone())?;
    let report = apply(&mut patcher, &opts)?;
    for n in &report.notes {
        println!("{n}");
    }
    let patched = patcher.into_image();
    let runs = ppf::diff_runs(&original, &patched);
    let bytes = ppf::write_ppf3("Legend of Legaia: play as a boss (battle model)", &runs);
    std::fs::write(out, &bytes)?;
    println!("wrote {out}: {} records, {} bytes", runs.len(), bytes.len());
    Ok(())
}

/// `render <disc> <out_dir> [--monster N] [--scale F] [--yaw DEG] [--frames K]
/// [--retail]`: apply the swap in memory and dump every clip as PNG strips.
fn render(args: &[String]) -> Result<()> {
    let disc = std::path::Path::new(&args[1]);
    let out = std::path::Path::new(&args[2]);
    let mut opts = Options::default();
    let (mut yaw, mut frames, mut retail, mut cell) = (35f32, 6usize, false, 200usize);
    let mut equip = [0u8; 5];
    let mut only: Vec<u32> = Vec::new();
    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--monster" => {
                opts.monster = args[i + 1].parse()?;
                i += 2;
            }
            "--scale" => {
                opts.scale = args[i + 1].parse()?;
                i += 2;
            }
            "--yaw" => {
                yaw = args[i + 1].parse()?;
                i += 2;
            }
            "--frames" => {
                frames = args[i + 1].parse()?;
                i += 2;
            }
            "--equip" => {
                for (k, v) in args[i + 1].split(',').enumerate().take(5) {
                    equip[k] = v.parse()?;
                }
                i += 2;
            }
            "--only" => {
                only = args[i + 1]
                    .split(',')
                    .map(|v| v.parse())
                    .collect::<Result<_, _>>()?;
                i += 2;
            }
            "--cell" => {
                cell = args[i + 1].parse()?;
                i += 2;
            }
            "--no-fireball" => {
                opts.fireball_art = None;
                opts.fireball_swing = false;
                opts.direct_cast = false;
                i += 1;
            }
            "--fireball-art" => {
                opts.fireball_art = Some(args.get(i + 1).context("--fireball-art NAME")?.clone());
                opts.fireball_swing = false;
                opts.direct_cast = false;
                i += 2;
            }
            "--no-direct-cast" => {
                opts.direct_cast = false;
                i += 1;
            }
            "--no-fireball-swing" => {
                opts.fireball_swing = false;
                opts.direct_cast = false;
                i += 1;
            }
            "--no-up-melee" => {
                opts.up_melee = false;
                i += 1;
            }
            "--cast-art" => {
                opts.cast_arts
                    .push(args.get(i + 1).context("--cast-art NAME")?.clone());
                i += 2;
            }
            "--keep-voices" => {
                opts.mute_host_voice = false;
                i += 1;
            }
            "--keep-support-anims" => {
                opts.calm_support = false;
                i += 1;
            }
            "--free-fireball" => {
                opts.fireball_damage = false;
                i += 1;
            }
            "--retail" => {
                retail = true;
                i += 1;
            }
            other => bail!("unknown argument {other}"),
        }
    }
    let resolved = legaia_iso::raw::resolve_disc_path(disc)?;
    let mut patcher = DiscPatcher::open(std::fs::read(&resolved)?)?;
    if !retail {
        let report = apply(&mut patcher, &opts)?;
        for n in &report.notes {
            println!("{n}");
        }
    }
    let player = patcher.read_player_file(legaia_boss_swap::VAHN_ENTRY)?;
    let readef = patcher.read_entry_footprint(legaia_boss_swap::READEF_ENTRY)?;
    for line in legaia_boss_swap::render::render_clips(
        &player, &readef, 0, out, frames, cell, yaw, equip, &only,
    )? {
        println!("{line}");
    }
    Ok(())
}

/// `monster <disc> <out.png> <id> [--only a,b]`: the unmodified monster, rest
/// pose, for comparing against a swapped build.
/// `clips <disc> ID`: every animation of a monster with the head fields the
/// stage plan reads, and the spells its record lists.
fn clips(args: &[String]) -> Result<()> {
    let resolved = legaia_iso::raw::resolve_disc_path(std::path::Path::new(&args[1]))?;
    let patcher = DiscPatcher::open(std::fs::read(&resolved)?)?;
    let archive = patcher.read_entry_footprint(legaia_boss_swap::MONSTER_ARCHIVE_ENTRY)?;
    let id: u16 = args[2].parse()?;
    let block = legaia_actor_formats::monster_archive::decode_block(&archive, id)?
        .context("empty monster slot")?;
    println!(
        "monster {id}: spells {:02x?}, record +0x20..0x28 {:02x?}",
        block.get(0x21..0x24).unwrap_or(&[]),
        block.get(0x20..0x28).unwrap_or(&[])
    );
    for c in legaia_party_swap::playerize::boss::boss_clips(&archive, id)? {
        let h = &c.head;
        let fx: Vec<String> = (0..8)
            .map(|r| 0x14 + r * 8)
            .filter(|&o| h[o] != 0 || h[o + 1] != 0)
            .map(|o| format!("f{}:{:02x?}", h[o], &h[o + 1..o + 8]))
            .collect();
        let cues: Vec<String> = (0x54..0x74)
            .step_by(4)
            .filter(|&o| h[o + 2] != 0 || h[o + 3] != 0)
            .map(|o| {
                format!(
                    "f{}:{:#x}",
                    u16::from_le_bytes([h[o], h[o + 1]]),
                    u16::from_le_bytes([h[o + 2], h[o + 3]])
                )
            })
            .collect();
        println!(
            "entry {:#04x} tag {:#04x} frames {:3} rate {} events {:?} window {:?} fx {:?} cues {:?}",
            c.entry,
            c.tag,
            c.len(),
            h[0x78],
            c.events(),
            &h[0x84..0x87],
            fx,
            cues
        );
    }
    Ok(())
}

fn monster(args: &[String]) -> Result<()> {
    use legaia_actor_formats::{mesh_raster, monster_archive};
    let resolved = legaia_iso::raw::resolve_disc_path(std::path::Path::new(&args[1]))?;
    let patcher = DiscPatcher::open(std::fs::read(&resolved)?)?;
    let archive = patcher.read_entry_footprint(legaia_boss_swap::MONSTER_ARCHIVE_ENTRY)?;
    let id: u16 = args[3].parse()?;
    let only: Vec<u32> = match args.get(4).map(String::as_str) {
        Some("--only") => args[5]
            .split(',')
            .map(|v| v.parse())
            .collect::<Result<_, _>>()?,
        _ => Vec::new(),
    };
    let mesh = monster_archive::mesh(&archive, id)?.context("empty slot")?;
    let mut vram = legaia_tim::Vram::new();
    let mut vm = mesh.battle_render_mesh(0, &mut vram).context("mesh")?;
    let tmd = legaia_tmd::parse(mesh.tmd_bytes()).context("tmd")?;
    let (_, ids, _) = legaia_tmd::mesh::tmd_to_vram_mesh_with_prim_ids(&tmd, mesh.tmd_bytes());
    let anims = monster_archive::animations(&archive, id)?.context("no anims")?;
    let frame = &anims[0].frames[0];
    let poses: Vec<mesh_raster::Pose> = frame
        .iter()
        .map(|p| mesh_raster::Pose::from_keyframe([p.tx, p.ty, p.tz], [p.rx, p.ry, p.rz]))
        .collect();
    // Probe: BOSS_FOREIGN_PAL=k repaints the prims whose nominal CLUT row or
    // page column is not the monster's own (row 481 / column 5) with palette k
    // on the monster's own page.
    if let Some(k) = std::env::var("BOSS_FOREIGN_PAL")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
    {
        let orig = legaia_tmd::mesh::tmd_to_vram_mesh(&tmd, mesh.tmd_bytes());
        let foreign: Vec<bool> = orig
            .cba_tsb
            .iter()
            .map(|ct| (ct[0] >> 6) != 481 || (ct[1] & 0xF) != 5)
            .collect();
        if k == 99 {
            // drop the foreign prims altogether
            vm.indices = vm
                .indices
                .chunks(3)
                .filter(|t| t.iter().all(|&i| !foreign[i as usize]))
                .flatten()
                .copied()
                .collect();
        }
        for (i, &is_foreign) in foreign.iter().enumerate() {
            if k != 99 && is_foreign {
                vm.cba_tsb[i][0] = monster_archive::relocate_cba(k, 0);
                vm.cba_tsb[i][1] = monster_archive::relocate_tsb(0x25, 0);
            }
        }
    }
    if !only.is_empty() {
        let keep: Vec<u32> = vm
            .indices
            .chunks(3)
            .filter(|t| t.iter().all(|&i| only.contains(&ids[i as usize])))
            .flatten()
            .copied()
            .collect();
        vm.indices = keep;
    }
    let opts = mesh_raster::RasterOptions {
        width: 600,
        height: 600,
        yaw: std::env::var("BOSS_YAW")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(20.0)
            .to_radians(),
        pitch: -10f32.to_radians(),
        margin: 0.04,
        background: [40, 44, 56, 255],
        shade: 0.0,
        auto_orient: false,
    };
    let img = mesh_raster::render_posed(&vm, &ids, &poses, &vram, &opts);
    let f = std::fs::File::create(&args[2])?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), 600, 600);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&img)?;
    Ok(())
}
