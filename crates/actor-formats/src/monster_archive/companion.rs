//! The **companion texture page** of the late bosses.
//!
//! Monster ids `187..=194` are not monsters: each archive slot holds one plain
//! TIM - a 256-colour CLUT authored for row 482 and a 4bpp `64 x 256`
//! halfword page authored for texture column 6. They are the second texture
//! page of ids `179..=186` (Songi's third form, the Corts, Koru): those
//! models carry prims authored one CLUT row and one page column past their own
//! (`cba` row 482, `tsb` column 6), and the battle loader (`FUN_80055468`)
//! *adds* the slot number to both, so in a lone-boss fight they read
//! `slot + 1`'s page and CLUT row. A retail VRAM capture of Cort's first form
//! shows the companion TIM there byte for byte (page) and with the STP bit set
//! on every non-zero CLUT entry. The mapping is `companion id = boss id + 8`.

use super::SLOT_STRIDE;
use anyhow::{Result, bail};

/// Offset from a boss id to its companion TIM's id.
pub const COMPANION_ID_OFFSET: u16 = 8;
/// First id of the TIM-only slots.
pub const COMPANION_FIRST_ID: u16 = 187;

/// A companion TIM: 16 palettes and a 4bpp page of `width x 256` indices.
#[derive(Debug, Clone)]
pub struct CompanionPage {
    /// Sixteen 16-colour CLUTs (the CLUT row's 256 entries, no STP bit).
    pub cluts: Vec<[u16; 16]>,
    /// Texel indices, `width` per row, 256 rows.
    pub indices: Vec<u8>,
    pub width: usize,
}

/// The companion page of boss `id`, if its slot (`id + 8`) holds a TIM.
pub fn companion_page(archive: &[u8], id: u16) -> Result<Option<CompanionPage>> {
    let cid = id + COMPANION_ID_OFFSET;
    if cid < COMPANION_FIRST_ID {
        return Ok(None);
    }
    let at = (usize::from(cid) - 1) * SLOT_STRIDE;
    let Some(slot) = archive.get(at..at + SLOT_STRIDE) else {
        return Ok(None);
    };
    let u32_at = |o: usize| u32::from_le_bytes(slot[o..o + 4].try_into().unwrap()) as usize;
    let u16_at = |o: usize| u16::from_le_bytes(slot[o..o + 2].try_into().unwrap()) as usize;
    if u32_at(0) != 0x10 || u32_at(4) & 0xF != 8 {
        return Ok(None);
    }
    let clut_len = u32_at(8);
    let (cw, ch) = (u16_at(8 + 8), u16_at(8 + 10));
    if ch != 1 || cw != 256 || clut_len != 12 + cw * 2 {
        bail!("companion TIM of id {id}: unexpected CLUT block {cw}x{ch}");
    }
    let cluts = (0..16)
        .map(|p| {
            let mut pal = [0u16; 16];
            for (i, c) in pal.iter_mut().enumerate() {
                *c = u16_at(8 + 12 + (p * 16 + i) * 2) as u16;
            }
            pal
        })
        .collect();
    let io = 8 + clut_len;
    let (iw_hw, ih) = (u16_at(io + 8), u16_at(io + 10));
    if ih != 256 || iw_hw == 0 || iw_hw > 64 {
        bail!("companion TIM of id {id}: unexpected image {iw_hw}x{ih}");
    }
    let pixels = slot
        .get(io + 12..io + 12 + iw_hw * 2 * ih)
        .ok_or_else(|| anyhow::anyhow!("companion TIM of id {id}: image past its slot"))?;
    let width = iw_hw * 4;
    let mut indices = vec![0u8; width * ih];
    for (i, &b) in pixels.iter().enumerate() {
        indices[i * 2] = b & 0xF;
        indices[i * 2 + 1] = b >> 4;
    }
    Ok(Some(CompanionPage {
        cluts,
        indices,
        width,
    }))
}
