//! Offline look at a rebuilt player file: every clip of the assembled battle
//! model rendered through the software rasteriser, as PNG strips.

use anyhow::{Context, Result};
use legaia_actor_formats::battle_char_assembly::loadout::ClipKind;
use legaia_actor_formats::battle_char_assembly::{self, loadout};
use legaia_actor_formats::mesh_raster::{self, Pose, RasterOptions};
use std::path::Path;

fn write_png(path: &Path, w: usize, h: usize, rgba: &[u8]) -> Result<()> {
    let f = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(rgba)?;
    Ok(())
}

/// Render `frames_per_clip` evenly spaced frames of every clip into one PNG
/// strip per clip under `out_dir`.
pub fn render_clips(
    player_file: &[u8],
    readef: &[u8],
    cslot: usize,
    out_dir: &Path,
    frames_per_clip: usize,
    cell: usize,
    yaw_deg: f32,
) -> Result<Vec<String>> {
    std::fs::create_dir_all(out_dir)?;
    // Art clips straight from the (patched) readef archives.
    let rec0 = battle_char_assembly::decode_record0(player_file)?;
    let bank = battle_char_assembly::art_animation_bank(&rec0)?;
    let mut arts = Vec::new();
    for rec in &bank {
        let Ok(archive) =
            battle_char_assembly::art_me_archive(readef, cslot, rec.uses_base_archive())
        else {
            continue;
        };
        let Ok(anim) = battle_char_assembly::art_animation(rec, &archive) else {
            continue;
        };
        arts.push(loadout::ArtClip {
            name: format!(
                "art{:02}{}{}",
                rec.index,
                if rec.uses_base_archive() { "_base" } else { "" },
                if rec.name.is_empty() {
                    String::new()
                } else {
                    format!("_{}", rec.name.replace(' ', "-"))
                }
            ),
            kind: "regular",
            ap: 0,
            directions: rec.combo.clone(),
            anim_id: rec.anim_id,
            segments: 1,
            anim,
        });
    }
    let c = loadout::build(player_file, cslot, [0; 5], false, &arts)
        .map_err(|e| anyhow::anyhow!("assemble: {e}"))?;
    let opts = RasterOptions {
        width: cell,
        height: cell,
        yaw: yaw_deg.to_radians(),
        pitch: -10f32.to_radians(),
        margin: 0.04,
        background: [40, 44, 56, 255],
        shade: 0.0,
        auto_orient: false,
    };
    let mut written = Vec::new();
    // Rest-pose extents (idle frame 0) so scale can be compared across builds.
    if let Some(frame) = c.clips.first().and_then(|k| k.anim.frames.first()) {
        let poses: Vec<Pose> = frame
            .iter()
            .map(|p| Pose::from_keyframe([p.tx, p.ty, p.tz], [p.rx, p.ry, p.rz]))
            .collect();
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for (v, &o) in c.mesh.positions.iter().zip(&c.object_ids) {
            if let Some(p) = poses.get(o as usize) {
                let w = p.apply(*v);
                for a in 0..3 {
                    lo[a] = lo[a].min(w[a]);
                    hi[a] = hi[a].max(w[a]);
                }
            }
        }
        written.push(format!(
            "note: rest extents x {:.0} y {:.0} z {:.0}",
            hi[0] - lo[0],
            hi[1] - lo[1],
            hi[2] - lo[2]
        ));
    }
    // One shared frame: the rest pose fixes the camera so clips are comparable.
    for (ci, clip) in c.clips.iter().enumerate() {
        let n = clip.anim.frames.len();
        if n == 0 {
            continue;
        }
        let k = frames_per_clip.min(n).max(1);
        let mut strip = vec![0u8; cell * k * cell * 4];
        for s in 0..k {
            let f = if k == 1 { 0 } else { s * (n - 1) / (k - 1) };
            let poses: Vec<Pose> = clip.anim.frames[f]
                .iter()
                .map(|p| Pose::from_keyframe([p.tx, p.ty, p.tz], [p.rx, p.ry, p.rz]))
                .collect();
            let img = mesh_raster::render_posed(&c.mesh, &c.object_ids, &poses, &c.vram, &opts);
            for y in 0..cell {
                let dst = (y * k * cell + s * cell) * 4;
                strip[dst..dst + cell * 4].copy_from_slice(&img[y * cell * 4..(y + 1) * cell * 4]);
            }
        }
        let kind = match clip.kind {
            ClipKind::Action => "act",
            ClipKind::Swing => "swing",
            ClipKind::Art => "art",
        };
        let name = format!(
            "{ci:02}_{kind}_{}_f{n}.png",
            clip.label.replace([' ', '/'], "-")
        );
        write_png(&out_dir.join(&name), cell * k, cell, &strip)
            .with_context(|| format!("write {name}"))?;
        written.push(name);
    }
    for n in &c.notes {
        written.push(format!("note: {n}"));
    }
    Ok(written)
}
