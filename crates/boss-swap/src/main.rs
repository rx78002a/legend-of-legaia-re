use anyhow::{Context, Result, bail};
use legaia_boss_swap::{Options, apply};
use legaia_disc_patch::disc::DiscPatcher;
use legaia_disc_patch::ppf;

fn usage() -> ! {
    eprintln!(
        "usage: boss-swap build <disc.bin|.cue> <out.ppf> [--monster N] [--scale F] [--classic] [--keep-voices]\n       \
         (writes a PPF 3.0 patch; the disc image is never modified)"
    );
    std::process::exit(2);
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("monster") && args.len() >= 4 {
        return monster(&args);
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
            "--classic" => {
                opts.fireball = false;
                i += 1;
            }
            "--keep-voices" => {
                opts.mute_host_voice = false;
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
            "--classic" => {
                opts.fireball = false;
                i += 1;
            }
            "--keep-voices" => {
                opts.mute_host_voice = false;
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
        yaw: 20f32.to_radians(),
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
