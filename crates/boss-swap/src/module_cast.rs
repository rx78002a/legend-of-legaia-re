//! Stage 2 for bosses whose specials are **cast-module cinematics**.
//!
//! A capture-class spell (record class byte `'c'`, `+1` = module sub-id) runs
//! its choreography from a per-spell overlay paged into slot B (cast-module.md).
//! Songi's second form owns two: Hyper Crush `0x4B` (PROT 936) and Hyper
//! Lightning `0x4C` (PROT 937). They were written for a monster caster; this
//! module routes the host's arts into them and fixes what a party caster
//! breaks. Both read the victim off `caster[+0x1DD]` and build the camera and
//! every effect from the caster's own position and facing, so they are far
//! closer to side-agnostic than the Delilas modules were. What is left:
//!
//! 1. **Entry.** The arts queue hook after the Super-Art applier
//!    (`0x801EF9AC`, one `jal`): a stub scans the finished queue for a starter
//!    (`0x19` / `0x1A`) followed by a cast-art constant and rewrites the action
//!    into category 2 (Magic) with the spell id at `+0x1DF`, the chosen target
//!    kept at `+0x1DD`. State `0x28` then sees a `'c'` spell and pages the
//!    module. The spell's MP cost is credited to the host first, so the debit
//!    (`0x801E4584`, an unsigned halfword subtract) cannot wrap. The
//!    spell-name banner gate is `delilas_cast::install_cast_label_gate`.
//! 2. **Caster stage rows.** The modules stage the caster's clips by raw
//!    action index (`actor+0x1DA`): `0x0D` then `0x0B` (936) or `0x0C` (937).
//!    On a party caster those resolve through the 12-word table at the head of
//!    record[0]: index `0x0D` / `0x0C` read past it and `0x0B` is the Block
//!    clip the stage boundary chokes on. The two `addiu` immediates become rows
//!    `0x0A` and `0x06`, which the player file authors as stage poses
//!    (`boss::stage_entry`).
//! 3. **Victim sweeps (936 only).** The damage sweep (phase 7) and the settle
//!    wait (phase 8) walk the *party* row `actor_table[0 .. ctx[+0]]` -
//!    victims of a monster caster. Each loop head is redirected to a helper
//!    that picks the opposite side of the caster: seats `3 .. 3 + ctx[+1]` for
//!    a party caster, `0 .. ctx[+0]` otherwise, so the retail fight against the
//!    real Songi keeps its behaviour.
//! 4. **Dead victims.** Both modules park until a dead victim reaches played id
//!    `8` (the party defeat pose). A monster corpse never does; the wait is
//!    removed and the end-of-action liveness sweep runs the real death.

use crate::asm::Asm;
use anyhow::{Context, Result, bail};
use legaia_code_hooks::arenas::{ARENA1_END_VA, ARENA1_VA, SCUS_GAP_END_VA, SCUS_GAP_VA};
use legaia_code_hooks::mips::{
    self as m, A0, A1, RA, S2, S3, SP, T0, T1, T2, T3, T4, T5, T6, T7, T8, V0, V1, ZERO,
};
use legaia_disc_patch::disc::DiscPatcher;

const SCUS_NAME: &str = "SCUS_942.54";
/// The applier call in the queue builder and the applier it targets.
const APPLIER_CALL_VA: u32 = 0x801E_F9AC;
const APPLIER_VA: u32 = 0x801E_F9E4;
/// `DAT_801F696C`: the applier's own "a Super Art fired" flag.
const SUPER_FIRED_VA: u32 = 0x801F_696C;
const ACTOR_TABLE_VA: u32 = 0x801C_9370;
const CTX_PTR_VA: u32 = 0x8007_BD24;
/// Static spell table: 12-byte records, `+3` = MP cost.
const SPELL_TABLE_VA: u32 = 0x8007_54C8;
const MODULE_BASE: u32 = 0x801F_69D8;

/// First art constant and the number of constants the cast table covers.
pub const TABLE_BASE: u8 = 0x10;
pub const TABLE_LEN: usize = 0x21;
/// The two record[0] rows the stage immediates are redirected to.
pub const STAGE_ROW_A: u8 = 0x0A;
pub const STAGE_ROW_B: u8 = 0x06;

/// Hyper Crush and Hyper Lightning: the spell ids this route knows modules for.
pub const SPELL_HYPER_CRUSH: u8 = 0x4B;
pub const SPELL_HYPER_LIGHTNING: u8 = 0x4C;

/// One art routed into a cast module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Special {
    /// The art's queue constant (bank row + `0x10`).
    pub constant: u8,
    /// The capture-class spell it casts.
    pub spell: u8,
}

/// Where the ARENA1 pieces sit.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    pub table: u32,
    pub end: u32,
    pub start_a: u32,
    pub start_b: u32,
    pub credit: u32,
    pub tail: u32,
}

pub fn layout() -> Layout {
    let table = ARENA1_VA;
    let end = table + 0x24;
    let start_a = end + assemble_end().len() as u32;
    let start_b = start_a + assemble_start(Regs::A).len() as u32;
    let credit = start_b + assemble_start(Regs::B).len() as u32;
    let tail = credit + assemble_credit().len() as u32;
    Layout {
        table,
        end,
        start_a,
        start_b,
        credit,
        tail,
    }
}

fn table(specials: &[Special]) -> [u8; 0x24] {
    let mut t = [0u8; 0x24];
    for s in specials {
        let i = usize::from(s.constant.wrapping_sub(TABLE_BASE));
        if i < TABLE_LEN {
            t[i] = s.spell;
        }
    }
    t
}

fn words_to_bytes(w: Vec<u32>) -> Vec<u8> {
    w.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// The queue hook stub that replaces the `jal` to the Super-Art applier.
/// Seat 0 only (the host); `table_va` holds the constant -> spell table.
pub fn assemble_route(table_va: u32, credit_va: u32) -> Vec<u8> {
    let mut a = Asm::new();
    let (done, scan, chk, next, hit) = (a.label(), a.label(), a.label(), a.label(), a.label());
    a.op(m::addiu(SP, SP, 0xFFE8));
    a.op(m::sw(RA, SP, 0x14));
    a.op(m::sw(A0, SP, 0x10));
    a.op(m::sw(A1, SP, 0x0C));
    a.op(m::jal(APPLIER_VA));
    a.op(m::nop());
    a.op(m::lw(A0, SP, 0x10));
    a.op(m::lw(A1, SP, 0x0C));
    a.op(m::lui(T0, m::hi(SUPER_FIRED_VA)));
    a.op(m::lw(T0, T0, m::lo(SUPER_FIRED_VA)));
    a.op(m::sll(T4, A0, 2)); // (load-delay filler)
    a.br(done, move |o| m::bne(A1, ZERO, o)); // host = character 0
    a.op(m::nop());
    a.br(done, move |o| m::bne(T0, ZERO, o)); // a Super fired: leave it
    a.op(m::lui(V1, m::hi(ACTOR_TABLE_VA))); // delay
    a.op(m::addiu(V1, V1, m::lo(ACTOR_TABLE_VA)));
    a.op(m::addu(V1, V1, T4));
    a.op(m::lw(V1, V1, 0)); // actor
    a.op(m::addiu(T5, ZERO, 0x12)); // scan bound (load-delay filler)
    a.op(m::addiu(T6, V1, 0x1DF)); // queue cursor
    a.bind(scan);
    a.op(m::lbu(T7, T6, 0));
    a.op(m::addiu(T8, ZERO, 0x19));
    a.br(done, move |o| m::beq(T7, ZERO, o)); // terminator
    a.op(m::nop());
    a.br(chk, move |o| m::beq(T7, T8, o)); // known art
    a.op(m::addiu(T8, ZERO, 0x1A)); // delay
    a.br(next, move |o| m::bne(T7, T8, o)); // not a newly learned one either
    a.op(m::nop());
    a.bind(chk);
    a.op(m::lbu(T7, T6, 1)); // art constant
    a.op(m::nop());
    a.op(m::addiu(T8, T7, 0xFFF0));
    a.op(m::sltiu(T1, T8, TABLE_LEN as u16));
    a.br(next, move |o| m::beq(T1, ZERO, o));
    a.op(m::lui(T2, m::hi(table_va))); // delay
    a.op(m::addiu(T2, T2, m::lo(table_va)));
    a.op(m::addu(T2, T2, T8));
    a.op(m::lbu(T3, T2, 0)); // spell id
    a.op(m::nop());
    a.br(next, move |o| m::beq(T3, ZERO, o)); // not a cast art
    a.op(m::nop());
    a.br(hit, move |o| m::beq(ZERO, ZERO, o));
    a.op(m::nop());
    a.bind(next);
    a.op(m::addiu(T6, T6, 1));
    a.op(m::addiu(T5, T5, 0xFFFF));
    a.br(scan, move |o| m::bne(T5, ZERO, o));
    a.op(m::nop());
    a.br(done, move |o| m::beq(ZERO, ZERO, o));
    a.op(m::nop());
    a.bind(hit);
    a.op(m::addiu(T0, ZERO, 2));
    a.op(m::sb(T0, V1, 0x1DE)); // category = Magic
    a.op(m::sb(T3, V1, 0x1DF)); // spell id over the queue head
    a.op(m::jal(credit_va)); // MP credit (spell t3, actor v1)
    a.op(m::nop());
    a.bind(done);
    a.op(m::lw(RA, SP, 0x14));
    a.op(m::addiu(SP, SP, 0x18));
    a.op(m::jr(RA));
    a.op(m::nop());
    words_to_bytes(a.finish())
}

/// MP credit for a routed cast (`t3` = spell, `v1` = actor): the cast debits
/// the spell's table cost from an unsigned halfword, so the cost is added
/// first and the host's MP is left where it was. Clobbers `t0`, `t1`.
pub fn assemble_credit() -> Vec<u8> {
    let mut a = Asm::new();
    a.op(m::sll(T0, T3, 1));
    a.op(m::addu(T0, T0, T3));
    a.op(m::sll(T0, T0, 2)); // spell * 12
    a.op(m::lui(T1, m::hi(SPELL_TABLE_VA)));
    a.op(m::addiu(T1, T1, m::lo(SPELL_TABLE_VA)));
    a.op(m::addu(T0, T0, T1));
    a.op(m::lbu(T0, T0, 3)); // MP cost
    a.op(m::lhu(T1, V1, 0x150)); // current MP
    a.op(m::nop());
    a.op(m::addu(T1, T1, T0));
    a.op(m::jr(RA));
    a.op(m::sh(T1, V1, 0x150));
    words_to_bytes(a.finish())
}

/// Loop end for the victim sweeps: `v0` = one past the last victim seat, or
/// `0` when the opposite side is empty. Clobbers `t0`, `t1`.
pub fn assemble_end() -> Vec<u8> {
    let mut a = Asm::new();
    let ret = a.label();
    a.op(m::lui(T0, m::hi(CTX_PTR_VA)));
    a.op(m::lw(T0, T0, m::lo(CTX_PTR_VA)));
    a.op(m::nop());
    a.op(m::lbu(T1, T0, 0x13)); // caster seat
    a.op(m::lbu(V0, T0, 0)); // party count
    a.op(m::sltiu(T1, T1, 3));
    a.br(ret, move |o| m::beq(T1, ZERO, o)); // monster caster: victims = party
    a.op(m::nop());
    a.op(m::lbu(V0, T0, 1)); // party caster: monster count
    a.op(m::nop());
    a.br(ret, move |o| m::beq(V0, ZERO, o));
    a.op(m::nop());
    a.op(m::addiu(V0, V0, 3));
    a.bind(ret);
    a.op(m::jr(RA));
    a.op(m::nop());
    words_to_bytes(a.finish())
}

/// Which registers a sweep keeps its `(index, pointer)` pair in.
#[derive(Debug, Clone, Copy)]
pub enum Regs {
    /// Phase 7: `s3` index, `s2` pointer.
    A,
    /// Phase 8: `a0` index, `a1` pointer.
    B,
}

/// First victim seat and its `actor_table` pointer, into the sweep's own
/// registers. Clobbers `t0`, `t1`.
pub fn assemble_start(regs: Regs) -> Vec<u8> {
    let (idx, ptr) = match regs {
        Regs::A => (S3, S2),
        Regs::B => (A0, A1),
    };
    let mut a = Asm::new();
    let enemy = a.label();
    a.op(m::lui(T0, m::hi(CTX_PTR_VA)));
    a.op(m::lw(T0, T0, m::lo(CTX_PTR_VA)));
    a.op(m::nop());
    a.op(m::lbu(T1, T0, 0x13));
    a.op(m::lui(ptr, m::hi(ACTOR_TABLE_VA)));
    a.op(m::sltiu(T1, T1, 3));
    a.br(enemy, move |o| m::beq(T1, ZERO, o));
    a.op(m::addu(idx, ZERO, ZERO)); // delay: monster caster -> seat 0
    a.op(m::addiu(idx, ZERO, 3)); // party caster -> seat 3
    a.bind(enemy);
    a.op(m::sll(T1, idx, 2));
    a.op(m::addiu(ptr, ptr, m::lo(ACTOR_TABLE_VA)));
    a.op(m::jr(RA));
    a.op(m::addu(ptr, ptr, T1));
    words_to_bytes(a.finish())
}

/// One expect-verified word edit inside a module image.
#[derive(Debug, Clone, Copy)]
pub struct ModuleEdit {
    pub va: u32,
    pub expect: u32,
    pub replace: u32,
}

fn stage_imm(row: u8) -> u32 {
    m::addiu(V0, ZERO, u16::from(row))
}

/// PROT 936 (Hyper Crush) edits.
pub fn edits_936(l: &Layout) -> Vec<ModuleEdit> {
    let lbu_v0_v0 = 0x9042_0000; // lbu v0,0(v0)
    let lbu_v0_s2 = 0x9242_0000; // lbu v0,0(s2)
    let lui_v0_801d = 0x3C02_801D;
    vec![
        // caster stage rows: 0x0D -> row A, 0x0B (Block on a party) -> row B
        ModuleEdit {
            va: 0x801F_6B9C,
            expect: 0x2402_000D,
            replace: stage_imm(STAGE_ROW_A),
        },
        ModuleEdit {
            va: 0x801F_6EAC,
            expect: 0x2402_000B,
            replace: stage_imm(STAGE_ROW_B),
        },
        // phase 7 damage sweep: victim side
        ModuleEdit {
            va: 0x801F_7928,
            expect: lbu_v0_v0,
            replace: m::jal(l.end),
        },
        ModuleEdit {
            va: 0x801F_7938,
            expect: lui_v0_801d,
            replace: m::jal(l.start_a),
        },
        ModuleEdit {
            va: 0x801F_7A38,
            expect: lbu_v0_v0,
            replace: m::jal(l.end),
        },
        // phase 8 settle wait: victim side, a dead victim is done
        ModuleEdit {
            va: 0x801F_7B18,
            expect: lbu_v0_s2,
            replace: m::jal(l.end),
        },
        ModuleEdit {
            va: 0x801F_7B30,
            expect: lui_v0_801d,
            replace: m::jal(l.start_b),
        },
        ModuleEdit {
            va: 0x801F_7B70,
            expect: 0x1047_0002, // beq v0,a3,+2 (dead victim at played id 8)
            replace: 0x1000_0002,
        },
    ]
}

/// PROT 937 (Hyper Lightning) edits.
pub fn edits_937() -> Vec<ModuleEdit> {
    vec![
        ModuleEdit {
            va: 0x801F_6B34,
            expect: 0x2402_000D,
            replace: stage_imm(STAGE_ROW_A),
        },
        ModuleEdit {
            va: 0x801F_6E7C,
            expect: 0x2402_000C,
            replace: stage_imm(STAGE_ROW_B),
        },
        // dead victim: no wait for played id 8
        ModuleEdit {
            va: 0x801F_77E4,
            expect: 0x1462_000E, // bne v1,v0,0x801F7820
            replace: m::nop(),
        },
    ]
}

/// Module PROT entry and edits for a spell.
pub fn module_for(spell: u8, l: &Layout) -> Option<(usize, Vec<ModuleEdit>)> {
    match spell {
        SPELL_HYPER_CRUSH => Some((936, edits_936(l))),
        SPELL_HYPER_LIGHTNING => Some((937, edits_937())),
        _ => None,
    }
}

fn apply_edits(p: &mut DiscPatcher, prot: usize, edits: &[ModuleEdit]) -> Result<bool> {
    let entry = p
        .read_entry(prot)
        .with_context(|| format!("read PROT {prot}"))?;
    let at = |e: &ModuleEdit| (e.va - MODULE_BASE) as usize;
    let word = |off: usize| -> Result<u32> {
        Ok(u32::from_le_bytes(
            entry
                .get(off..off + 4)
                .with_context(|| format!("PROT {prot} short at +{off:#x}"))?
                .try_into()?,
        ))
    };
    let mut changed = false;
    let mut pending = Vec::new();
    for e in edits {
        let cur = word(at(e))?;
        if cur == e.replace {
            continue;
        }
        if cur != e.expect {
            bail!(
                "PROT {prot} {:#010x}: expected {:#010x}, found {:#010x}",
                e.va,
                e.expect,
                cur
            );
        }
        pending.push(e);
    }
    for e in pending {
        p.patch_prot_entry(prot, at(e) as u64, &e.replace.to_le_bytes())?;
        changed = true;
    }
    Ok(changed)
}

fn write_scus(patcher: &mut DiscPatcher, va: u32, bytes: &[u8]) -> Result<bool> {
    let scus = patcher
        .read_named_file(SCUS_NAME)
        .context("read SCUS_942.54")?;
    let off = legaia_game_tables::item_names::file_offset_for_va(&scus, va)
        .with_context(|| format!("resolve the file offset of {va:#010x}"))?;
    let cur = &scus[off..off + bytes.len()];
    if cur == bytes {
        return Ok(false);
    }
    if cur.iter().any(|&b| b != 0) {
        bail!("SCUS region at {va:#010x} is not free: another injected feature owns it");
    }
    patcher.patch_named_file(SCUS_NAME, off as u64, bytes)?;
    Ok(true)
}

/// Install the route for `specials`: the queue hook, its table and helpers,
/// the module edits and the spell-name banner gate. Returns whether anything
/// changed.
pub fn install(patcher: &mut DiscPatcher, specials: &[Special]) -> Result<bool> {
    if specials.is_empty() {
        bail!("module cast route needs at least one special");
    }
    let l = layout();
    let route = assemble_route(l.table, l.credit);
    if SCUS_GAP_VA + route.len() as u32 > SCUS_GAP_END_VA {
        bail!("module-cast route stub does not fit the SCUS gap");
    }
    if l.tail > ARENA1_END_VA {
        bail!("module-cast helpers do not fit ARENA1");
    }
    let mut changed = false;
    changed |= write_scus(patcher, SCUS_GAP_VA, &route)?;
    changed |= write_scus(patcher, l.table, &table(specials))?;
    changed |= write_scus(patcher, l.end, &assemble_end())?;
    changed |= write_scus(patcher, l.start_a, &assemble_start(Regs::A))?;
    changed |= write_scus(patcher, l.start_b, &assemble_start(Regs::B))?;
    changed |= write_scus(patcher, l.credit, &assemble_credit())?;
    changed |= legaia_delilas_party::delilas_cast::redirect_applier_call(patcher, SCUS_GAP_VA)?;
    changed |= legaia_delilas_party::delilas_cast::install_cast_label_gate(patcher)?;
    let _ = APPLIER_CALL_VA;
    let mut done = Vec::new();
    for s in specials {
        let (prot, edits) = module_for(s.spell, &l)
            .with_context(|| format!("no cast-module edits for spell {:#04x}", s.spell))?;
        if done.contains(&prot) {
            continue;
        }
        changed |= apply_edits(patcher, prot, &edits)?;
        done.push(prot);
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use legaia_code_hooks::mips_sim::Cpu;

    const ACTOR: u32 = 0x8010_0000;
    const CTX: u32 = 0x8011_0000;
    const RETURN_VA: u32 = 0x801E_FA00;
    const STACK: u32 = 0x801F_F000;
    const SPECIALS: [Special; 2] = [
        Special {
            constant: 0x27,
            spell: 0x4B,
        },
        Special {
            constant: 0x29,
            spell: 0x4C,
        },
    ];

    /// Run the route stub. Returns `(category, queue head, MP)`.
    fn route(char_idx: u32, super_fired: u32, queue: &[u8], mp: u16) -> (u8, u8, u16) {
        let mut cpu = Cpu::new();
        let l = layout();
        cpu.load(SCUS_GAP_VA, &assemble_route(l.table, l.credit));
        cpu.load(l.table, &table(&SPECIALS));
        cpu.load(l.credit, &assemble_credit());
        // the applier: returns at once
        cpu.load_words(APPLIER_VA, &[m::jr(RA), m::nop()]);
        cpu.wr32(ACTOR_TABLE_VA, ACTOR);
        cpu.wr32(SUPER_FIRED_VA, super_fired);
        cpu.wr8(ACTOR + 0x1DE, 3);
        cpu.load(ACTOR + 0x1DF, queue);
        cpu.wr16(ACTOR + 0x150, mp);
        // spell table: Hyper Crush costs 0xB4, Hyper Lightning 0x78
        cpu.wr8(SPELL_TABLE_VA + 0x4B * 12 + 3, 0xB4);
        cpu.wr8(SPELL_TABLE_VA + 0x4C * 12 + 3, 0x78);
        cpu.r[A0 as usize] = 0; // slot
        cpu.r[A1 as usize] = char_idx;
        cpu.r[SP as usize] = STACK;
        cpu.r[RA as usize] = RETURN_VA;
        cpu.pc = SCUS_GAP_VA;
        cpu.run_until(&[RETURN_VA]);
        assert_eq!(cpu.r[SP as usize], STACK, "stack balanced");
        (
            cpu.rd8(ACTOR + 0x1DE),
            cpu.rd8(ACTOR + 0x1DF),
            cpu.rd16(ACTOR + 0x150),
        )
    }

    #[test]
    fn a_cast_art_becomes_the_spell_with_its_mp_credited() {
        let (cat, head, mp) = route(0, 0, &[0x0F, 0x0E, 0x19, 0x27, 0], 42);
        assert_eq!((cat, head), (2, 0x4B));
        assert_eq!(mp, 42 + 0xB4, "debit of 180 leaves the MP where it was");
        let (cat, head, mp) = route(0, 0, &[0x0D, 0x1A, 0x29, 0], 7);
        assert_eq!((cat, head, mp), (2, 0x4C, 7 + 0x78));
    }

    #[test]
    fn everything_else_is_left_alone() {
        for (ch, sup, q) in [
            (1u32, 0u32, &[0x19u8, 0x27, 0][..]), // another character
            (0, 1, &[0x19, 0x27, 0][..]),         // a Super fired
            (0, 0, &[0x0F, 0x19, 0x1F, 0][..]),   // another art
            (0, 0, &[0x0F, 0x0E, 0x0D, 0][..]),   // plain swings
            (0, 0, &[0x19, 0x31, 0][..]),         // past the table
        ] {
            let (cat, head, mp) = route(ch, sup, q, 99);
            assert_eq!((cat, head, mp), (3, q[0], 99), "{ch} {sup} {q:x?}");
        }
    }

    fn side(caster: u8, party: u8, monsters: u8) -> Cpu {
        let mut cpu = Cpu::new();
        let l = layout();
        cpu.load(l.end, &assemble_end());
        cpu.load(l.start_a, &assemble_start(Regs::A));
        cpu.load(l.start_b, &assemble_start(Regs::B));
        cpu.wr32(CTX_PTR_VA, CTX);
        cpu.wr8(CTX + 0x13, caster);
        cpu.wr8(CTX, party);
        cpu.wr8(CTX + 1, monsters);
        cpu.r[RA as usize] = RETURN_VA;
        cpu
    }

    #[test]
    fn end_helper_picks_the_victim_side() {
        for (caster, party, mon, want) in [
            (0u8, 3u8, 2u8, 5u32), // party caster: seats 3..5
            (1, 2, 5, 8),
            (0, 3, 0, 0), // nobody to hit
            (3, 3, 2, 3), // monster caster: the party row 0..3
            (4, 1, 3, 1),
        ] {
            let mut cpu = side(caster, party, mon);
            cpu.pc = layout().end;
            cpu.run_until(&[RETURN_VA]);
            assert_eq!(cpu.r[V0 as usize], want, "caster {caster}");
        }
    }

    #[test]
    fn start_helpers_set_the_sweeps_own_registers() {
        for (caster, idx, ptr) in [
            (0u8, 3u32, ACTOR_TABLE_VA + 12),
            (2, 3, ACTOR_TABLE_VA + 12),
            (3, 0, ACTOR_TABLE_VA),
            (7, 0, ACTOR_TABLE_VA),
        ] {
            let mut cpu = side(caster, 3, 2);
            cpu.pc = layout().start_a;
            cpu.run_until(&[RETURN_VA]);
            assert_eq!((cpu.r[S3 as usize], cpu.r[S2 as usize]), (idx, ptr));
            let mut cpu = side(caster, 3, 2);
            cpu.pc = layout().start_b;
            cpu.run_until(&[RETURN_VA]);
            assert_eq!((cpu.r[A0 as usize], cpu.r[A1 as usize]), (idx, ptr));
        }
    }

    #[test]
    fn everything_fits() {
        let l = layout();
        assert!(l.tail <= ARENA1_END_VA, "{:#x}", l.tail);
        assert!(
            SCUS_GAP_VA as usize + assemble_route(l.table, l.credit).len()
                <= SCUS_GAP_END_VA as usize
        );
    }
}
