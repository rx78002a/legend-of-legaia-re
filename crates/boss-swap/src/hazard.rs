//! A load-delay hazard scan for injected MIPS.
//!
//! The R3000 has no interlock on loads: the instruction right after a load
//! still sees the register's old value. The test simulator does not model
//! that, so a stub or a patched module word that reads its own freshly loaded
//! register in the next slot passes every simulated check and misbehaves on
//! hardware. This scan finds those pairs.

/// Registers an instruction word reads (bit mask over `$0..$31`).
fn reads(w: u32) -> u32 {
    let (op, rs, rt) = (w >> 26, (w >> 21) & 31, (w >> 16) & 31);
    let bit = |r: u32| 1u32 << r;
    let m = match op {
        0 => match w & 0x3F {
            0 | 2 | 3 => bit(rt),           // sll srl sra
            8 | 9 | 0x11 | 0x13 => bit(rs), // jr jalr mthi mtlo
            0x0C | 0x0D | 0x10 | 0x12 => 0, // syscall break mfhi mflo
            _ => bit(rs) | bit(rt),         // the rest of SPECIAL
        },
        1 | 6 | 7 => bit(rs),                 // bltz.. blez bgtz
        2 | 3 | 0x0F => 0,                    // j jal lui
        4 | 5 => bit(rs) | bit(rt),           // beq bne
        0x08..=0x0E => bit(rs),               // immediate ALU
        0x20..=0x26 | 0x32 | 0x3A => bit(rs), // loads, lwc2, swc2
        0x28..=0x2E => bit(rs) | bit(rt),     // stores
        _ => 0,
    };
    m & !1 // $0 never carries a value
}

/// The register a plain load writes, if `w` is one.
fn load_target(w: u32) -> Option<u32> {
    match w >> 26 {
        0x20 | 0x21 | 0x23 | 0x24 | 0x25 => Some((w >> 16) & 31).filter(|&r| r != 0),
        _ => None,
    }
}

/// Indices `i` where word `i` loads a register that word `i + 1` reads.
pub fn load_hazards(words: &[u32]) -> Vec<usize> {
    (0..words.len().saturating_sub(1))
        .filter(|&i| load_target(words[i]).is_some_and(|r| reads(words[i + 1]) & (1 << r) != 0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use legaia_code_hooks::mips::{self as m, A0, A1, GP, S2, V0, ZERO};

    #[test]
    fn a_load_feeding_the_next_word_is_flagged() {
        let w = [m::lw(V0, GP, 0), m::addiu(S2, V0, 4), m::nop()];
        assert_eq!(load_hazards(&w), vec![0]);
        // a store of the loaded register reads it too
        let w = [m::lw(V0, GP, 0), m::sw(V0, GP, 4)];
        assert_eq!(load_hazards(&w), vec![0]);
    }

    #[test]
    fn a_filled_slot_is_clean() {
        let w = [
            m::lw(V0, GP, 0),
            m::addiu(A1, ZERO, 1),
            m::addiu(S2, V0, 4),
            m::lbu(A0, GP, 1),
            m::lw(V0, A0, 0), // reads a0 one word after its load: flagged
        ];
        assert_eq!(load_hazards(&w), vec![3]);
        // a load into $0 and an overwrite of the target are not hazards
        assert!(load_hazards(&[m::lw(ZERO, GP, 0), m::addiu(S2, ZERO, 1)]).is_empty());
        assert!(load_hazards(&[m::lw(V0, GP, 0), m::addiu(V0, ZERO, 1)]).is_empty());
    }
}
