//! The texture side of a playerized file: the party band regions and face-stamp
//! windows, palette merging, and re-laying a monster's texels out into the
//! band.

use super::super::*;
use super::CLUT_BYTES;
use legaia_actor_formats::battle_char_assembly::SECTION_TEXTURE_RECTS;

/// One free texture region of the party band, in authoring-page space.
struct BandRegion {
    /// Authoring page: 0 = texpage 0x15, 1 = 0x16.
    page: usize,
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
    /// Which equipment section's pool uploads this region.
    section: usize,
}

/// Per-character live-face window inside section 1's rect, in band
/// texel coordinates `(x0, y0, x1, y1)` (x1/y1 exclusive): the union of
/// the eye + mouth stamp destination rects from the static
/// `SCUS_942.54` face-frame tables (`DAT_800768CC` / `DAT_800768E4`,
/// halfword coords x4 - mirror `legaia_actor_formats::face_anim`, values
/// asserted against the disc by `face_stamp_windows_match_the_scus_tables`).
///
/// The per-frame facial animator (`FUN_8004C7B4`) MoveImages the
/// character's eye / mouth frames onto these rows EVERY frame - the
/// neutral frame is re-stamped even when no track record is active - so
/// any sibling texels the relayout parks here are overwritten with the
/// host's face the moment a battle starts (the source strips live in
/// `RECORD0_TEXTURE_RECTS[0]`'s tile, which keeps its retail pixels).
/// The relayout must therefore keep this window empty and unreferenced.
///
/// Vahn: eyes (0,16) 15x17 + mouth (4,42) 7x16; Noa: eyes (2,72) 16x32 +
/// mouth (8,104) 4x24; Gala: eyes (0,24) 14x24 + mouth (4,48) 6x24.
pub const FACE_STAMP_WINDOWS: [(usize, usize, usize, usize); 3] = [
    (0, 16, 15 * 4, 58),      // Vahn: hw 0..15, rows 16..58
    (2 * 4, 72, 18 * 4, 128), // Noa: hw 2..18, rows 72..128
    (0, 24, 14 * 4, 72),      // Gala: hw 0..14, rows 24..72
];

/// The section tiles in authoring-page space (see
/// `SECTION_TEXTURE_RECTS`: band halfword x < 0x40 = page 0x15, else
/// 0x16; texels = halfwords * 4). Section 1's tile is split around the
/// character's live-face window ([`FACE_STAMP_WINDOWS`]) so no sibling
/// cluster is placed where the facial animator stamps - the emitted
/// pool still covers the full rect (window texels stay zero).
fn band_regions(char_slot: usize) -> Vec<BandRegion> {
    let mut out = Vec::new();
    for (section, r) in SECTION_TEXTURE_RECTS.iter().enumerate() {
        let hw_x = r.x0 as usize;
        let (page, page_hw_x) = if hw_x < 0x40 {
            (0, hw_x)
        } else {
            (1, hw_x - 0x40)
        };
        let base = BandRegion {
            page,
            x0: page_hw_x * 4,
            y0: r.y0 as usize,
            w: r.w as usize * 4,
            h: r.h as usize,
            section,
        };
        let window = FACE_STAMP_WINDOWS.get(char_slot).copied();
        if section != 1 || window.is_none() {
            out.push(base);
            continue;
        }
        // The face window is authored band-relative and section 1's rect
        // starts at band (0, 0), so window coords are region-local.
        let (wx0, wy0, wx1, wy1) = window.unwrap();
        let (wx1, wy1) = (wx1.min(base.w), wy1.min(base.h));
        let mut push = |x0: usize, y0: usize, w: usize, h: usize| {
            if w > 0 && h > 0 {
                out.push(BandRegion {
                    page: base.page,
                    x0: base.x0 + x0,
                    y0: base.y0 + y0,
                    w,
                    h,
                    section,
                });
            }
        };
        push(0, 0, base.w, wy0); // above the window
        push(0, wy0, wx0, wy1 - wy0); // left of the window
        push(wx1, wy0, base.w - wx1, wy1 - wy0); // right of the window
        push(0, wy1, base.w, base.h - wy1); // below the window
    }
    out
}

/// Decode a monster pool into per-texel indices + CLUTs.
pub(in super::super) fn monster_pool_texels(
    pool: &[u8],
) -> Result<(Vec<[u16; 16]>, Vec<u8>, usize)> {
    if pool.len() <= CLUT_REGION_BYTES {
        bail!("monster pool too small");
    }
    let cluts: Vec<[u16; 16]> = (0..CLUT_COUNT)
        .map(|c| {
            let mut pal = [0u16; 16];
            for (i, slot) in pal.iter_mut().enumerate() {
                *slot = u16::from_le_bytes(
                    pool[c * CLUT_BYTES + i * 2..c * CLUT_BYTES + i * 2 + 2]
                        .try_into()
                        .unwrap(),
                );
            }
            pal
        })
        .collect();
    let pixels = &pool[CLUT_REGION_BYTES..];
    let bytes_per_row = pixels.len() / PAGE_HEIGHT;
    if bytes_per_row == 0 {
        bail!("monster pool has no pixel rows");
    }
    let width = bytes_per_row * 2;
    let mut indices = vec![0u8; width * PAGE_HEIGHT];
    for y in 0..PAGE_HEIGHT {
        for xb in 0..bytes_per_row {
            let b = pixels[y * bytes_per_row + xb];
            indices[y * width + xb * 2] = b & 0x0F;
            indices[y * width + xb * 2 + 1] = b >> 4;
        }
    }
    Ok((cluts, indices, width))
}

/// `(group_of_palette, group_colors)` - a texel's new index is its
/// colour's position in its group.
type PaletteGroups = (BTreeMap<u8, usize>, Vec<Vec<u16>>);

/// Union-merge the used palettes down to `budget` groups.
///
/// `idx0_opaque` counts a palette's index 0 as a colour when it is not
/// `0x0000` (a monster palette may keep a real colour there: the PSX GPU
/// makes a texel transparent by its CLUT value, not by its index). A palette
/// with sixteen distinct colours then loses the least distinct one, since a
/// band palette holds fifteen.
pub(in super::super) fn merge_palettes(
    used: &[u8],
    cluts: &[[u16; 16]],
    budget: usize,
    idx0_opaque: bool,
    warnings: &mut Vec<String>,
) -> Result<PaletteGroups> {
    // Start with one group per used palette (distinct colours, index 0
    // reserved for transparent 0x0000).
    let mut groups: Vec<(Vec<u8>, Vec<u16>)> = used
        .iter()
        .map(|&p| {
            let mut colors: Vec<u16> = Vec::new();
            for &c in cluts[p as usize].iter().skip(usize::from(!idx0_opaque)) {
                if c != 0 && !colors.contains(&c) {
                    colors.push(c);
                }
            }
            while colors.len() > 15 {
                let mut best = (usize::MAX, 1usize);
                for i in 0..colors.len() {
                    for j in (i + 1)..colors.len() {
                        let d = color_distance(colors[i], colors[j]);
                        if d < best.0 {
                            best = (d, j);
                        }
                    }
                }
                colors.remove(best.1);
            }
            (vec![p], colors)
        })
        .collect();
    // Greedy: repeatedly merge the pair with the smallest colour union
    // while over budget; prefer lossless unions (<= 15 non-transparent
    // colours), fall back to dropping the least-frequent colours.
    while groups.len() > budget {
        let mut best: Option<(usize, usize, usize)> = None;
        for i in 0..groups.len() {
            for j in (i + 1)..groups.len() {
                let mut union = groups[i].1.clone();
                for &c in &groups[j].1 {
                    if !union.contains(&c) {
                        union.push(c);
                    }
                }
                let cost = union.len();
                if best.is_none_or(|(_, _, b)| cost < b) {
                    best = Some((i, j, cost));
                }
            }
        }
        let Some((i, j, cost)) = best else { break };
        if cost > 15 {
            warnings.push(format!(
                "palette merge exceeds 15 colours ({cost}); nearest colours collapse"
            ));
        }
        let (pals_j, colors_j) = groups.remove(j);
        let gi = &mut groups[i];
        gi.0.extend(pals_j);
        for c in colors_j {
            if !gi.1.contains(&c) {
                gi.1.push(c);
            }
        }
        gi.1.truncate(15);
    }
    let mut group_of = BTreeMap::new();
    let mut colors = Vec::new();
    for (gidx, (pals, cols)) in groups.iter().enumerate() {
        for &p in pals {
            group_of.insert(p, gidx);
        }
        colors.push(cols.clone());
    }
    Ok((group_of, colors))
}

/// Squared RGB555 distance of two colours.
fn color_distance(a: u16, b: u16) -> usize {
    let split = |v: u16| {
        (
            (v & 0x1F) as i32,
            ((v >> 5) & 0x1F) as i32,
            ((v >> 10) & 0x1F) as i32,
        )
    };
    let ((ar, ag, ab), (br, bg, bb)) = (split(a), split(b));
    ((ar - br).pow(2) + (ag - bg).pow(2) + (ab - bb).pow(2)) as usize
}

/// Whether the texel `(x, y)` lies on the polygon a prim's UVs outline (quads
/// are two triangles in the file's Z order), with three quarters of a texel of
/// slack so the texels the rasteriser touches along an edge count.
fn covers(uvs: &[(u8, u8)], x: usize, y: usize) -> bool {
    let p = (x as f32 + 0.5, y as f32 + 0.5);
    let q = |i: usize| (f32::from(uvs[i].0), f32::from(uvs[i].1));
    let inside = |a: (f32, f32), b: (f32, f32), c: (f32, f32)| {
        let edge = |u: (f32, f32), v: (f32, f32)| {
            let (ex, ey) = (v.0 - u.0, v.1 - u.1);
            let len = (ex * ex + ey * ey).sqrt().max(1e-3);
            (ex * (p.1 - u.1) - ey * (p.0 - u.0)) / len
        };
        let area = (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
        let sign = if area < 0.0 { -1.0 } else { 1.0 };
        [edge(a, b), edge(b, c), edge(c, a)]
            .iter()
            .all(|d| d * sign >= -0.75)
    };
    match uvs.len() {
        3 => inside(q(0), q(1), q(2)),
        4 => inside(q(0), q(1), q(2)) || inside(q(1), q(3), q(2)),
        _ => true,
    }
}

/// Nearest colour in `colors` to `c` (RGB555 distance).
pub(in super::super) fn nearest_color(colors: &[u16], c: u16) -> usize {
    let split = |v: u16| {
        (
            (v & 0x1F) as i32,
            ((v >> 5) & 0x1F) as i32,
            ((v >> 10) & 0x1F) as i32,
        )
    };
    let (r, g, b) = split(c);
    let mut best = 0usize;
    let mut best_d = i32::MAX;
    for (i, &o) in colors.iter().enumerate() {
        let (orr, og, ob) = split(o);
        let d = (r - orr).pow(2) + (g - og).pow(2) + (b - ob).pow(2);
        if d < best_d {
            best_d = d;
            best = i;
        }
    }
    best
}

/// Palette ids at and above this belong to the companion page (a second
/// source page whose 16 CLUTs sit at `COMPANION_PAL_BASE..`), so a prim of it
/// never shares an island, a palette group or a texel with the primary page.
pub(super) const COMPANION_PAL_BASE: u16 = 32;

fn face_palette(objects: &[ModelObject], f: &FaceRef) -> u16 {
    objects[f.obj].groups[f.group].prims[f.prim].cba & 0x3F
}

/// An island's footprint in the band at horizontal / vertical divisors
/// `sx` / `sy`.
fn island_dims(bbox: (u8, u8, u8, u8), sx: u32, sy: u32) -> (usize, usize) {
    (
        ((bbox.2 - bbox.0) as usize + 1).div_ceil(sx as usize),
        ((bbox.3 - bbox.1) as usize + 1).div_ceil(sy as usize),
    )
}

/// The band texels an island occupies: those its faces' polygons cover,
/// widened by `ring` texels all round (what a rasteriser that overshoots a
/// polygon's edge may read), in a `(w + 2 * ring) x (h + 2 * ring)` frame whose
/// content starts at `(ring, ring)`.
struct IslandMask {
    w: usize,
    h: usize,
    cells: Vec<bool>,
}

fn island_mask(
    cluster: &Cluster,
    sy: u32,
    ring: usize,
    covered: &[Vec<(usize, usize)>],
) -> IslandMask {
    let (w, h) = island_dims(cluster.bbox, cluster.scale, sy);
    let (mw, mh) = (w + 2 * ring, h + 2 * ring);
    let mut core = vec![false; mw * mh];
    let (bx, by) = (cluster.bbox.0 as usize, cluster.bbox.1 as usize);
    for &fi in &cluster.faces {
        for &(x, y) in &covered[fi] {
            let tx = (x - bx) / cluster.scale as usize + ring;
            let ty = (y - by) / sy as usize + ring;
            core[ty * mw + tx] = true;
        }
    }
    let mut cells = core.clone();
    for y in 0..mh {
        for x in 0..mw {
            if !core[y * mw + x] {
                continue;
            }
            for yy in y.saturating_sub(ring)..=(y + ring).min(mh - 1) {
                for xx in x.saturating_sub(ring)..=(x + ring).min(mw - 1) {
                    cells[yy * mw + xx] = true;
                }
            }
        }
    }
    IslandMask {
        w: mw,
        h: mh,
        cells,
    }
}

/// Pack island masks into the band `regions`, first fit with the largest
/// first; a mask only has to avoid the *texels* the others occupy, so a small
/// island settles in the gaps of a large one. `allowed(island, region)` gates
/// where an island may go. Returns `(region, x, y)` of each mask's frame, `None`
/// for one that found no room.
fn pack_masks(
    regions: &[BandRegion],
    masks: &[IslandMask],
    allowed: impl Fn(usize, usize) -> bool,
) -> Vec<Option<(usize, usize, usize)>> {
    let mut taken: Vec<Vec<bool>> = regions.iter().map(|r| vec![false; r.w * r.h]).collect();
    let mut order: Vec<usize> = (0..masks.len()).collect();
    order.sort_by_key(|&i| {
        let m = &masks[i];
        let n = m.cells.iter().filter(|&&c| c).count();
        (std::cmp::Reverse(m.w.max(m.h)), std::cmp::Reverse(n), i)
    });
    let mut placed = vec![None; masks.len()];
    for i in order {
        let m = &masks[i];
        let pts: Vec<(usize, usize)> = (0..m.h)
            .flat_map(|y| (0..m.w).map(move |x| (x, y)))
            .filter(|&(x, y)| m.cells[y * m.w + x])
            .collect();
        'regions: for (ri, r) in regions.iter().enumerate() {
            if !allowed(i, ri) || m.w > r.w || m.h > r.h {
                continue;
            }
            for y in 0..=(r.h - m.h) {
                'spot: for x in 0..=(r.w - m.w) {
                    for &(px, py) in &pts {
                        if taken[ri][(y + py) * r.w + x + px] {
                            continue 'spot;
                        }
                    }
                    for &(px, py) in &pts {
                        taken[ri][(y + py) * r.w + x + px] = true;
                    }
                    placed[i] = Some((ri, x, y));
                    break 'regions;
                }
            }
        }
    }
    placed
}

/// The re-laid band texture: per-section pool blocks + per-face rewrites.
pub(super) struct BandLayout {
    /// Per-section `[u16 clut_x][u16 clut_n][cluts][pixels]` pool bytes.
    pub(super) section_pools: Vec<Vec<u8>>,
}

/// Re-layout the Delilas page into the five section tiles, rewriting the
/// baked objects' UV/CBA/TSB in place (player authoring conventions:
/// CLUT row 480, texpages 0x15/0x16).
#[allow(clippy::too_many_arguments)]
pub(super) fn relayout_to_band(
    objects: &mut [ModelObject],
    cluts: &[[u16; 16]],
    indices: &[u8],
    src_width: usize,
    companion: Option<(&[u8], usize)>,
    idx0_opaque: bool,
    page1_only: &[usize],
    reserved_cols: &[u16],
    exclude_section: Option<usize>,
    base_scale: u32,
    char_slot: usize,
    warnings: &mut Vec<String>,
) -> Result<BandLayout> {
    // Collect textured faces (source page is single: the monster page).
    let mut faces: Vec<FaceRef> = Vec::new();
    for (oi, o) in objects.iter().enumerate() {
        for (gi, g) in o.groups.iter().enumerate() {
            if !g.shape.is_textured() {
                continue;
            }
            for (pi, p) in g.prims.iter().enumerate() {
                faces.push(FaceRef {
                    obj: oi,
                    group: gi,
                    prim: pi,
                    page: usize::from(companion.is_some() && (p.cba & 0x3F) >= COMPANION_PAL_BASE),
                    bbox: face_bbox(&p.uvs),
                });
            }
        }
    }

    // Cluster faces whose polygons touch: a texel one covers lies within
    // `TOUCH` texels of one the other covers. Boxes would join charts that only
    // overlap in the artists' padding and leave half of every island empty.
    // Faces on different source palettes never share an island: the same
    // texels recoloured through two CLUTs (palette swaps) would otherwise
    // collide on one band tile and the second palette's faces would sample
    // the first's index values.
    const TOUCH: i32 = 2;
    // the source texels each face's polygon covers (its corner when none)
    let covered: Vec<Vec<(usize, usize)>> = faces
        .iter()
        .map(|f| {
            let uvs = &objects[f.obj].groups[f.group].prims[f.prim].uvs;
            let mut texels = Vec::new();
            for y in f.bbox.1..=f.bbox.3 {
                for x in f.bbox.0..=f.bbox.2 {
                    if covers(uvs, x as usize, y as usize) {
                        texels.push((x as usize, y as usize));
                    }
                }
            }
            if texels.is_empty() {
                texels.push((f.bbox.0 as usize, f.bbox.1 as usize));
            }
            texels
        })
        .collect();
    let mut parent: Vec<usize> = (0..faces.len()).collect();
    fn find(parent: &mut Vec<usize>, i: usize) -> usize {
        if parent[i] != i {
            let r = find(parent, parent[i]);
            parent[i] = r;
        }
        parent[i]
    }
    let mut seen: std::collections::HashMap<(u16, i32, i32), usize> =
        std::collections::HashMap::new();
    for i in 0..faces.len() {
        let pal = face_palette(objects, &faces[i]);
        for &(x, y) in &covered[i] {
            let (x, y) = (x as i32, y as i32);
            for dy in -TOUCH..=TOUCH {
                for dx in -TOUCH..=TOUCH {
                    if let Some(&j) = seen.get(&(pal, x + dx, y + dy)) {
                        let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                        if ri != rj {
                            parent[ri] = rj;
                        }
                    }
                }
            }
        }
        for &(x, y) in &covered[i] {
            seen.entry((pal, x as i32, y as i32)).or_insert(i);
        }
    }
    let mut clusters: BTreeMap<usize, Cluster> = BTreeMap::new();
    #[allow(clippy::needless_range_loop)]
    for i in 0..faces.len() {
        let root = find(&mut parent, i);
        let e = clusters.entry(root).or_insert(Cluster {
            faces: Vec::new(),
            page: 0,
            bbox: faces[i].bbox,
            scale: base_scale,
            dst: (0, 0),
        });
        e.bbox = union_bbox(e.bbox, faces[i].bbox);
        e.faces.push(i);
    }
    let mut clusters: Vec<Cluster> = clusters.into_values().collect();
    // A cluster holding any face of a `page1_only` object is placed on page 1
    // (authoring texpage `0x16`) only: the battle loader's TSB relocation maps
    // `0x15` to the slot's first page and every other page to its second, and
    // an object whose primitive region two object-table entries share is
    // relocated twice - which sends a page-0 face to page 1 and leaves a
    // page-1 face where it is.
    let page1: Vec<bool> = clusters
        .iter()
        .map(|c| {
            c.faces
                .iter()
                .any(|&fi| page1_only.contains(&faces[fi].obj))
        })
        .collect();

    // Multi-region pack: place each cluster into one band region (a face
    // samples a single texpage, so a cluster cannot straddle a region
    // boundary). Islands are packed by the texels their polygons cover (see
    // [`pack_masks`]). On overflow the largest island is halved along its
    // longer axis and the pack retried.
    let regions = band_regions(char_slot);
    // vertical divisor per cluster (`Cluster::scale` is the horizontal one)
    let mut scale_y: Vec<u32> = vec![base_scale; clusters.len()];
    // (region, x, y) per cluster, and the ring width
    let (placement, ring): (Vec<(usize, usize, usize)>, usize) = loop {
        // Try with a one-texel ring round every island's polygons first (a
        // rasteriser that overshoots a face's edge then reads the island's own
        // colours, not a neighbour's), then without when the band is too tight.
        let attempt = |ring: usize| -> Vec<Option<(usize, usize, usize)>> {
            let masks: Vec<IslandMask> = clusters
                .iter()
                .zip(&scale_y)
                .map(|(c, &sy)| island_mask(c, sy, ring, &covered))
                .collect();
            pack_masks(&regions, &masks, |i, ri| {
                let r = &regions[ri];
                // a `page1_only` island sits on page 1 only; the weapon
                // section's tile belongs to the per-record weapon texture
                // (see `weapon_fuse`) - no sibling islands there
                !(page1[i] && r.page != 1) && Some(r.section) != exclude_section
            })
        };
        let mut found = None;
        for ring in [1usize, 0] {
            let placed = attempt(ring);
            if placed.iter().all(|p| p.is_some()) {
                found = Some((placed.into_iter().map(|p| p.unwrap()).collect(), ring));
                break;
            }
        }
        if let Some(f) = found {
            break f;
        }
        let cap = base_scale * 4;
        let Some(ci) = (0..clusters.len())
            .filter(|&i| clusters[i].scale < cap || scale_y[i] < cap)
            .max_by_key(|&i| {
                let (w, h) = island_dims(clusters[i].bbox, clusters[i].scale, scale_y[i]);
                w * h
            })
        else {
            bail!("texture islands exceed the party band tiles even at quarter resolution");
        };
        let c = &mut clusters[ci];
        let (w, h) = island_dims(c.bbox, c.scale, scale_y[ci]);
        // halve the longer axis (the other when that one is at its floor)
        if (w >= h && c.scale < cap) || scale_y[ci] >= cap {
            c.scale *= 2;
        } else {
            scale_y[ci] *= 2;
        }
        warnings.push(format!(
            "player-side texture island {}x{} downscaled to 1/{} x 1/{} to fit the band",
            c.bbox.2 - c.bbox.0 + 1,
            c.bbox.3 - c.bbox.1 + 1,
            c.scale,
            scale_y[ci]
        ));
    };
    // the band texels each island occupies (its polygons plus the ring)
    let masks: Vec<IslandMask> = clusters
        .iter()
        .zip(&scale_y)
        .map(|(c, &sy)| island_mask(c, sy, ring, &covered))
        .collect();

    // Palette groups within the free-column budget.
    let mut used: Vec<u8> = faces
        .iter()
        .map(|f| (objects[f.obj].groups[f.group].prims[f.prim].cba & 0x3F) as u8)
        .collect();
    used.sort_unstable();
    used.dedup();
    let free_cols: Vec<u16> = (0..16u16).filter(|c| !reserved_cols.contains(c)).collect();
    if free_cols.is_empty() {
        bail!("record[0] claims every CLUT column of the band row");
    }
    let (group_of, group_colors) =
        merge_palettes(&used, cluts, free_cols.len(), idx0_opaque, warnings)?;
    if group_colors.len() > free_cols.len() {
        bail!(
            "{} palette groups but only {} free CLUT columns",
            group_colors.len(),
            free_cols.len()
        );
    }
    let col_of_group: Vec<u16> = free_cols[..group_colors.len()].to_vec();

    // Paint the band tiles: per cluster, copy texel colours through the
    // source palette, then re-index through the face's group palette.
    // A texel shared by faces of two groups keeps the first writer
    // (bounded seam residual - same limitation the OBJ importer has).
    let mut tiles: Vec<Vec<u8>> = regions.iter().map(|r| vec![0u8; r.w * r.h]).collect();
    let mut painted: Vec<Vec<bool>> = regions.iter().map(|r| vec![false; r.w * r.h]).collect();
    // painted from a texel the face's polygon covers (a later covered texel
    // may replace one that was only a stand-in)
    let mut hard: Vec<Vec<bool>> = regions.iter().map(|r| vec![false; r.w * r.h]).collect();
    let src_texel = |page: usize, x: usize, y: usize| -> u8 {
        let (ind, w) = match (page, companion) {
            (1, Some(c)) => c,
            _ => (indices, src_width),
        };
        if x >= w || y >= PAGE_HEIGHT {
            return 0;
        }
        ind[y * w + x]
    };
    for (ci, c) in clusters.iter().enumerate() {
        let (ri, dx, dy) = placement[ci];
        let (sx, sy) = (c.scale as isize, scale_y[ci] as isize);
        let mask = &masks[ci];
        let ring_i = ring as isize;
        for &fi in &c.faces {
            let f = &faces[fi];
            let p = &objects[f.obj].groups[f.group].prims[f.prim];
            let src_pal = (p.cba & 0x3F) as usize;
            let group = group_of[&(src_pal as u8)];
            let colors = &group_colors[group];
            let fb = f.bbox;
            let (bx, by) = (c.bbox.0 as isize, c.bbox.1 as isize);
            let r = &regions[ri];
            // The band texels the face can sample: the rewritten UVs are
            // `(uv - bbox0) / s`, so it reaches `(fb0 - bbox0) / s ..=
            // (fb2 - bbox0) / s`, and the ring beyond it. At full resolution
            // each takes the source texel under it (first writer wins). A
            // downscaled island picks per band texel a source texel the
            // face's polygon really covers: the box around a triangle holds
            // the artists' padding (bright checkers), and a block-centre
            // sample that landed in it showed up as stray coloured dots along
            // the face's edges.
            let tx_range = (fb.0 as isize - bx) / sx - ring_i..=(fb.2 as isize - bx) / sx + ring_i;
            let ty_range = (fb.1 as isize - by) / sy - ring_i..=(fb.3 as isize - by) / sy + ring_i;
            let mut picks: Vec<(isize, isize, usize, usize, bool)> = Vec::new();
            for ty0 in ty_range.clone() {
                for tx0 in tx_range.clone() {
                    let (mx, my) = (tx0 + ring_i, ty0 + ring_i);
                    if mx < 0
                        || my < 0
                        || mx >= mask.w as isize
                        || my >= mask.h as isize
                        || !mask.cells[my as usize * mask.w + mx as usize]
                    {
                        continue;
                    }
                    let (bsx, bsy) = (bx + tx0 * sx, by + ty0 * sy);
                    let (cx, cy) = (bsx + sx / 2, bsy + sy / 2);
                    // the covered source texel of this block nearest its centre
                    let (x0, x1) = (bsx.max(fb.0 as isize), (bsx + sx - 1).min(fb.2 as isize));
                    let (y0, y1) = (bsy.max(fb.1 as isize), (bsy + sy - 1).min(fb.3 as isize));
                    let mut covered_texel: Option<(isize, isize, isize)> = None;
                    for y in y0..=y1 {
                        for x in x0..=x1 {
                            if !covers(&p.uvs, x as usize, y as usize) {
                                continue;
                            }
                            let d = (x - cx).abs() + (y - cy).abs();
                            if covered_texel.is_none_or(|(bd, _, _)| d < bd) {
                                covered_texel = Some((d, x, y));
                            }
                        }
                    }
                    match covered_texel {
                        Some((_, x, y)) => picks.push((tx0, ty0, x as usize, y as usize, true)),
                        None => {
                            let x = cx.clamp(fb.0 as isize, fb.2 as isize) as usize;
                            let y = cy.clamp(fb.1 as isize, fb.3 as isize) as usize;
                            picks.push((tx0, ty0, x, y, false));
                        }
                    }
                }
            }
            // texels past the polygon's edge copy the nearest covered one
            let covered_picks: Vec<(isize, isize, usize, usize)> = picks
                .iter()
                .filter(|p| p.4)
                .map(|p| (p.0, p.1, p.2, p.3))
                .collect();
            for pick in picks.iter_mut().filter(|p| !p.4) {
                let near = covered_picks
                    .iter()
                    .min_by_key(|h| (h.0 - pick.0).abs().max((h.1 - pick.1).abs()));
                if let Some(&(_, _, x, y)) = near {
                    pick.2 = x;
                    pick.3 = y;
                }
            }
            for (tx0, ty0, sx_, sy_, hard_pick) in picks {
                let (tx, ty) = (dx + (tx0 + ring_i) as usize, dy + (ty0 + ring_i) as usize);
                if tx >= r.w || ty >= r.h {
                    continue;
                }
                let at = ty * r.w + tx;
                if painted[ri][at] && !(hard_pick && !hard[ri][at]) {
                    continue;
                }
                let idx = src_texel(f.page, sx_, sy_) as usize;
                let color = cluts[src_pal][idx];
                let new_idx = if (idx == 0 && !idx0_opaque) || color == 0 {
                    0
                } else {
                    match colors.iter().position(|&cc| cc == color) {
                        Some(pos) => pos + 1,
                        None => nearest_color(colors, color) + 1,
                    }
                };
                tiles[ri][at] = new_idx as u8;
                painted[ri][at] = true;
                hard[ri][at] = hard_pick;
            }
        }
    }
    // Rewrite the faces' UV / CBA / TSB.
    for (ci, c) in clusters.iter().enumerate() {
        let (ri, dx, dy) = placement[ci];
        let r = &regions[ri];
        let (sx, sy) = (c.scale as usize, scale_y[ci] as usize);
        for &fi in &c.faces {
            let f = &faces[fi];
            let p = &mut objects[f.obj].groups[f.group].prims[f.prim];
            let src_pal = (p.cba & 0x3F) as u8;
            let group = group_of[&src_pal];
            let col = col_of_group[group];
            for uv in p.uvs.iter_mut() {
                let nx = r.x0 + dx + ring + (uv.0 as usize - c.bbox.0 as usize) / sx;
                let ny = r.y0 + dy + ring + (uv.1 as usize - c.bbox.1 as usize) / sy;
                *uv = (nx.min(UV_SPACE - 1) as u8, ny.min(PAGE_HEIGHT - 1) as u8);
            }
            // Authoring CBA: CLUT row 480, column * 16 halfwords.
            p.cba = (480u16 << 6) | col;
            // Authoring TSB: page 0x15/0x16, ABR bits preserved, 4bpp.
            let page = if r.page == 0 { 0x15u16 } else { 0x16 };
            p.tsb = (p.tsb & 0x0060) | page;
        }
    }

    // Build the per-section pool blocks. Section 0 carries the whole
    // palette run; the others upload pixels only. A section may be split
    // into several sub-regions (the live-face window of section 1) - the
    // emitted pool always covers the section's FULL rect, with each
    // sub-region's tile blitted at its offset and unplaced texels (the
    // face window) left zero.
    let mut section_pools: Vec<Vec<u8>> = Vec::with_capacity(SECTION_TEXTURE_RECTS.len());
    for (section, rect) in SECTION_TEXTURE_RECTS.iter().enumerate() {
        let hw_x = rect.x0 as usize;
        let sec_x0 = if hw_x < 0x40 { hw_x } else { hw_x - 0x40 } * 4;
        let sec_y0 = rect.y0 as usize;
        let (sec_w, sec_h) = (rect.w as usize * 4, rect.h as usize);
        let mut full = vec![0u8; sec_w * sec_h];
        for (ri, r) in regions.iter().enumerate() {
            if r.section != section {
                continue;
            }
            let (dx, dy) = (r.x0 - sec_x0, r.y0 - sec_y0);
            let tile = &tiles[ri];
            for y in 0..r.h {
                let dst = (dy + y) * sec_w + dx;
                full[dst..dst + r.w].copy_from_slice(&tile[y * r.w..(y + 1) * r.w]);
            }
        }
        let mut block = Vec::new();
        if section == 0 {
            // One contiguous run covering our columns is not guaranteed
            // (free columns may be scattered), so emit the run from the
            // lowest to the highest used column, holes zero-filled -
            // zero entries upload as transparent and nothing samples
            // the hole columns.
            let lo = *col_of_group.iter().min().unwrap();
            let hi = *col_of_group.iter().max().unwrap();
            let n = (hi - lo + 1) * 16;
            block.extend_from_slice(&(lo * 16).to_le_bytes());
            block.extend_from_slice(&n.to_le_bytes());
            let mut run = vec![0u16; n as usize];
            for (g, colors) in group_colors.iter().enumerate() {
                let base = ((col_of_group[g] - lo) * 16) as usize;
                for (i, &c) in colors.iter().enumerate() {
                    run[base + 1 + i] = c;
                }
            }
            for w in run {
                block.extend_from_slice(&w.to_le_bytes());
            }
        } else {
            block.extend_from_slice(&0u16.to_le_bytes());
            block.extend_from_slice(&0u16.to_le_bytes());
        }
        // Pixels: pack the tile's texel indices, low nibble first.
        for y in 0..sec_h {
            for xb in 0..sec_w / 2 {
                let lo = full[y * sec_w + xb * 2] & 0xF;
                let hi = full[y * sec_w + xb * 2 + 1] & 0xF;
                block.push(lo | (hi << 4));
            }
        }
        section_pools.push(block);
    }
    Ok(BandLayout { section_pools })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(w: usize, h: usize) -> BandRegion {
        BandRegion {
            page: 0,
            x0: 0,
            y0: 0,
            w,
            h,
            section: 0,
        }
    }

    /// An L: a full left column and a full bottom row of a `w x h` frame.
    fn ell(w: usize, h: usize) -> IslandMask {
        let mut cells = vec![false; w * h];
        for y in 0..h {
            for x in 0..w {
                cells[y * w + x] = x == 0 || y == h - 1;
            }
        }
        IslandMask { w, h, cells }
    }

    fn block(w: usize, h: usize) -> IslandMask {
        IslandMask {
            w,
            h,
            cells: vec![true; w * h],
        }
    }

    #[test]
    fn a_small_island_settles_in_the_gap_of_a_large_one() {
        // an 8x8 L leaves a 7x7 hole; a 6x6 block cannot go beside it in an
        // 8x8 region unless it takes the hole
        let regions = [region(8, 8)];
        let masks = [ell(8, 8), block(6, 6)];
        let placed = pack_masks(&regions, &masks, |_, _| true);
        assert_eq!(placed[0], Some((0, 0, 0)));
        assert_eq!(placed[1], Some((0, 1, 0)));
    }

    #[test]
    fn overlapping_islands_do_not_share_texels() {
        let regions = [region(8, 4)];
        let masks = [block(5, 4), block(5, 4)];
        let placed = pack_masks(&regions, &masks, |_, _| true);
        assert_eq!(placed.iter().filter(|p| p.is_some()).count(), 1);
    }

    #[test]
    fn the_ring_widens_a_covered_texel() {
        let mut c = Cluster {
            faces: vec![0],
            page: 0,
            bbox: (10, 10, 12, 12),
            scale: 1,
            dst: (0, 0),
        };
        let covered = vec![vec![(11usize, 11usize)]];
        let m = island_mask(&c, 1, 1, &covered);
        assert_eq!((m.w, m.h), (5, 5));
        assert_eq!(m.cells.iter().filter(|&&b| b).count(), 9);
        c.scale = 2;
        let m = island_mask(&c, 1, 0, &covered);
        assert_eq!((m.w, m.h), (2, 3));
        assert_eq!(m.cells.iter().filter(|&&b| b).count(), 1);
    }
}
