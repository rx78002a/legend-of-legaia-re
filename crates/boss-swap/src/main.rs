use anyhow::{Context, Result, bail};
use legaia_boss_swap::{Options, apply};
use legaia_disc_patch::disc::DiscPatcher;
use legaia_disc_patch::ppf;

fn usage() -> ! {
    eprintln!(
        "usage: boss-swap build <disc.bin|.cue> <out.ppf> [--monster N] [--scale F]\n       \
         (writes a PPF 3.0 patch; the disc image is never modified)"
    );
    std::process::exit(2);
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
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
    let (mut yaw, mut frames, mut retail) = (35f32, 6usize, false);
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
    for line in legaia_boss_swap::render::render_clips(&player, &readef, 0, out, frames, 200, yaw)?
    {
        println!("{line}");
    }
    Ok(())
}
