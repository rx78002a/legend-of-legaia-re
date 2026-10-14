//! Fitting a monster's parts onto the player rig's fifteen channels.
//!
//! A player battle model animates [`CANONICAL_PARTS`] rigid parts; a monster's
//! mesh has anywhere from one to twenty-one. Fewer is easy: the unused
//! channels carry zero poses and the missing objects are empty. More is not:
//! every part needs a channel of its own, so the parts past fifteen are
//! *merged* into the part whose motion they follow best.
//!
//! A part is posed rigidly, `world = R(pose) * v + t(pose)`. Merging part `j`
//! into part `k` re-expresses `j`'s vertices in `k`'s frame at the idle rest
//! pose, so the rest pose is exact, and `j` then moves with `k`. The merge
//! target is the kept part that minimises how far `j`'s vertices stray from
//! that rigid-follow prediction over every frame of every clip, so a tail
//! segment joins the segment before it and a shoulder plate joins the arm.

use super::boss::BossClip;
use crate::{CANONICAL_PARTS, rot_matrix};
use legaia_actor_formats::monster_archive::PartPose;
use legaia_tmd::encode::ModelObject;

/// How a monster's parts map onto the player's channels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartFit {
    /// Source parts kept, in canonical order (at most [`CANONICAL_PARTS`]).
    pub keep: Vec<usize>,
    /// `(dropped part, index into keep)`: the part is merged into that one.
    pub merges: Vec<(usize, usize)>,
}

impl PartFit {
    /// A monster that needs no change.
    pub fn is_identity(&self, parts: usize) -> bool {
        parts == CANONICAL_PARTS && self.merges.is_empty()
    }
}

type Mat = [[f32; 3]; 3];

fn mul_vec(m: &Mat, v: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

fn mul_vec_t(m: &Mat, v: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * v[0] + m[1][0] * v[1] + m[2][0] * v[2],
        m[0][1] * v[0] + m[1][1] * v[1] + m[2][1] * v[2],
        m[0][2] * v[0] + m[1][2] * v[1] + m[2][2] * v[2],
    ]
}

fn world(p: &PartPose, v: [f32; 3]) -> [f32; 3] {
    let r = mul_vec(&rot_matrix(p), v);
    [
        r[0] + f32::from(p.tx),
        r[1] + f32::from(p.ty),
        r[2] + f32::from(p.tz),
    ]
}

/// `v` (in `j`'s frame) re-expressed in `k`'s frame, both posed by `rest`.
fn into_frame(rest: &[PartPose], j: usize, k: usize, v: [f32; 3]) -> [f32; 3] {
    let w = world(&rest[j], v);
    let t = [
        w[0] - f32::from(rest[k].tx),
        w[1] - f32::from(rest[k].ty),
        w[2] - f32::from(rest[k].tz),
    ];
    mul_vec_t(&rot_matrix(&rest[k]), t)
}

fn prim_count(o: &ModelObject) -> usize {
    o.groups.iter().map(|g| g.prims.len()).sum()
}

/// Up to `n` of an object's vertices, evenly spaced.
fn samples(o: &ModelObject, n: usize) -> Vec<[f32; 3]> {
    let step = o.vertices.len().div_ceil(n.max(1)).max(1);
    o.vertices
        .iter()
        .step_by(step)
        .map(|v| [f32::from(v[0]), f32::from(v[1]), f32::from(v[2])])
        .collect()
}

/// The idle rest pose: frame 0 of the first idle clip (tag 0), else of the
/// first clip.
fn rest_pose(clips: &[BossClip]) -> Option<&[PartPose]> {
    let c = clips
        .iter()
        .find(|c| c.tag == 0)
        .or_else(|| clips.first())?;
    c.frames.first().map(Vec::as_slice)
}

/// Plan the fit of `objects` (the monster's, with `clips` posing them) onto
/// the player rig.
pub fn plan(objects: &[ModelObject], clips: &[BossClip]) -> PartFit {
    let n = objects.len();
    if n <= CANONICAL_PARTS {
        return PartFit {
            keep: (0..n).collect(),
            merges: Vec::new(),
        };
    }
    // keep the largest parts, in their own order
    let mut by_size: Vec<usize> = (0..n).collect();
    by_size.sort_by_key(|&i| (std::cmp::Reverse(prim_count(&objects[i])), i));
    let mut keep: Vec<usize> = by_size[..CANONICAL_PARTS].to_vec();
    keep.sort_unstable();
    let dropped: Vec<usize> = by_size[CANONICAL_PARTS..].to_vec();
    let Some(rest) = rest_pose(clips).filter(|r| r.len() >= n) else {
        // no poses to judge by: follow the previous part
        let merges = dropped
            .into_iter()
            .map(|j| (j, keep.iter().rposition(|&k| k < j).unwrap_or(0)))
            .collect();
        return PartFit { keep, merges };
    };
    let mut merges = Vec::new();
    for j in dropped {
        let pts = samples(&objects[j], 24);
        let mut best = (f32::INFINITY, 0usize);
        for (slot, &k) in keep.iter().enumerate() {
            let rel: Vec<[f32; 3]> = pts.iter().map(|&v| into_frame(rest, j, k, v)).collect();
            let mut cost = 0.0f32;
            for clip in clips {
                let step = clip.frames.len().div_ceil(40).max(1);
                for frame in clip.frames.iter().step_by(step) {
                    if frame.len() <= j.max(k) {
                        continue;
                    }
                    for (v, r) in pts.iter().zip(&rel) {
                        let a = world(&frame[j], *v);
                        let b = world(&frame[k], *r);
                        cost += (a[0] - b[0]).abs() + (a[1] - b[1]).abs() + (a[2] - b[2]).abs();
                    }
                }
            }
            if cost < best.0 {
                best = (cost, slot);
            }
        }
        merges.push((j, best.1));
    }
    PartFit { keep, merges }
}

/// The monster's objects on the player rig: kept parts in canonical order,
/// the dropped parts folded into their targets, padded with empty objects to
/// [`CANONICAL_PARTS`].
pub fn fit_objects(
    objects: Vec<ModelObject>,
    fit: &PartFit,
    clips: &[BossClip],
) -> Vec<ModelObject> {
    let rest = rest_pose(clips).filter(|r| r.len() >= objects.len());
    let mut out: Vec<ModelObject> = fit.keep.iter().map(|&i| objects[i].clone()).collect();
    for &(j, slot) in &fit.merges {
        let k = fit.keep[slot];
        let src = &objects[j];
        let base = out[slot].vertices.len() as u16;
        for v in &src.vertices {
            let p = [f32::from(v[0]), f32::from(v[1]), f32::from(v[2])];
            let q = match rest {
                Some(rest) => into_frame(rest, j, k, p),
                None => p,
            };
            let r = |x: f32| x.round().clamp(-32768.0, 32767.0) as i16;
            out[slot].vertices.push([r(q[0]), r(q[1]), r(q[2])]);
        }
        for g in &src.groups {
            let mut g = g.clone();
            for prim in &mut g.prims {
                for v in &mut prim.vertices {
                    *v += base;
                }
            }
            match out[slot]
                .groups
                .iter_mut()
                .find(|h| h.shape == g.shape && h.semi_transparent == g.semi_transparent)
            {
                Some(h) => h.prims.extend(g.prims),
                None => out[slot].groups.push(g),
            }
        }
    }
    while out.len() < CANONICAL_PARTS {
        out.push(ModelObject {
            vertices: Vec::new(),
            groups: Vec::new(),
            scale: legaia_tmd::encode::LEGAIA_OBJECT_SCALE,
        });
    }
    out
}

/// The clips' poses for the kept parts only (a dropped part rides its
/// target's channel).
pub fn fit_clips(clips: &mut [BossClip], fit: &PartFit) {
    for clip in clips {
        for frame in &mut clip.frames {
            let kept: Vec<PartPose> = fit
                .keep
                .iter()
                .map(|&i| frame.get(i).copied().unwrap_or_default())
                .collect();
            *frame = kept;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use legaia_tmd::descriptor::PacketShape;
    use legaia_tmd::encode::{LEGAIA_OBJECT_SCALE, ModelGroup, ModelPrim};

    fn object(prims: usize, at: [i16; 3]) -> ModelObject {
        let prim = ModelPrim {
            vertices: vec![0, 1, 2],
            uvs: Vec::new(),
            cba: 0,
            tsb: 0,
            colors: vec![[128, 128, 128]],
        };
        ModelObject {
            vertices: vec![at, [at[0] + 8, at[1], at[2]], [at[0], at[1] + 8, at[2]]],
            groups: vec![ModelGroup {
                shape: PacketShape::F3,
                semi_transparent: false,
                prims: vec![prim; prims],
            }],
            scale: LEGAIA_OBJECT_SCALE,
        }
    }

    fn pose(tx: i16, rz: u16) -> PartPose {
        PartPose {
            tx,
            ty: 0,
            tz: 0,
            rx: 0,
            ry: 0,
            rz,
        }
    }

    fn clip(frames: Vec<Vec<PartPose>>) -> BossClip {
        BossClip {
            entry: 0,
            tag: 0,
            head: vec![0; 0x8C],
            frames,
        }
    }

    #[test]
    fn a_small_monster_is_padded_with_empty_objects() {
        let objs = vec![object(2, [0, 0, 0]), object(1, [10, 0, 0])];
        let clips = vec![clip(vec![vec![pose(0, 0), pose(5, 0)]])];
        let fit = plan(&objs, &clips);
        assert_eq!(fit.keep, vec![0, 1]);
        let out = fit_objects(objs, &fit, &clips);
        assert_eq!(out.len(), CANONICAL_PARTS);
        assert!(out[2..].iter().all(|o| o.vertices.is_empty()));
    }

    #[test]
    fn a_part_past_fifteen_joins_the_part_it_follows() {
        // sixteen parts: part 15 is a one-prim plate that rides part 4
        let mut objs: Vec<ModelObject> =
            (0..16).map(|i| object(5, [i as i16 * 10, 0, 0])).collect();
        objs[15] = object(1, [0, 0, 0]);
        let frames: Vec<Vec<PartPose>> = (0..6)
            .map(|f| {
                let mut row: Vec<PartPose> = (0..16)
                    .map(|i| pose(i as i16 * 7 + f * 3, (f as u16) * 90 * (i as u16 % 5)))
                    .collect();
                row[15] = row[4];
                row
            })
            .collect();
        let clips = vec![clip(frames)];
        let fit = plan(&objs, &clips);
        assert_eq!(fit.keep.len(), CANONICAL_PARTS);
        assert_eq!(fit.merges.len(), 1);
        let (j, slot) = fit.merges[0];
        assert_eq!(j, 15);
        assert_eq!(fit.keep[slot], 4, "the plate follows part 4");
        let out = fit_objects(objs, &fit, &clips);
        assert_eq!(out.len(), CANONICAL_PARTS);
        // part 4 gained the plate's three vertices and its prim
        assert_eq!(out[slot].vertices.len(), 6);
        assert_eq!(out[slot].groups[0].prims.len(), 6);
        // the plate sits where it did at rest: identical poses, identical
        // frames, so the vertices come back unchanged but for 4's own pose
        let mut c = clips;
        fit_clips(&mut c, &fit);
        assert_eq!(c[0].frames[0].len(), CANONICAL_PARTS);
    }
}
