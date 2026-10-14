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
//!    module. The spell-name banner gate is `delilas_cast::install_cast_label_gate`.
//! 2. **Cast prep** (`0x801E4584`, the MP debit of state `0x28`). One helper
//!    runs for every caster of every spell and does three things: it writes
//!    the **sweep scratch** (below); for a routed party cast it adds the spell's
//!    cost back so the unsigned halfword debit cannot wrap (credited at queue
//!    time the MP was reset by the round setup before the cast ran: the cost
//!    has to be neutralised where it is spent); and it points record[0] row
//!    `0x06` at the stage pose that spell's second stage wants.
//! 3. **Caster stage rows.** The modules stage the caster's clips by raw
//!    action index (`actor+0x1DA`): `0x0D` then `0x0B` (936) or `0x0C` (937).
//!    On a party caster those resolve through the 12-word table at the head of
//!    record[0]: `0x0D` / `0x0C` read past it and `0x0B` is the Block clip the
//!    stage boundary chokes on. The `addiu` immediates become rows `0x0A` and
//!    `0x06`. Row `0x0A` holds the boss's `0x0D` entry; row `0x06` is
//!    **repointed per cast** by the prep helper onto the pool entry (an extra
//!    stage pose stored below the table, outside it) for the spell being cast,
//!    its `+0x88` stream pointer rebuilt the way the record loader does. The
//!    table has too few spare rows for three distinct poses.
//! 4. **Victim sweeps.** 936's damage sweep (phase 7) and settle wait (phase 8)
//!    walk the party row `actor_table[0 .. ctx[+0]]`, and 937's phase 2 hides
//!    the party row (retail: the heroes it is about to hit, while the caster
//!    performs). A party caster must hit and hide the *monster* row instead.
//!    The prep helper stores the opposite side's `(start, end, 8 - count,
//!    pointers)` in a scratch block in ARENA2 and each loop head reads it with
//!    a single `gp`-relative load: seats `3 .. 3 + ctx[+1]` for a party caster,
//!    `0 .. ctx[+0]` otherwise, so the retail fight against the real Songi keeps
//!    its behaviour.
//! 5. **Dead victims.** Both modules park until a dead victim reaches played id
//!    `8` (the party defeat pose). A monster corpse never does; the wait is
//!    removed and the end-of-action liveness sweep runs the real death.

use crate::asm::Asm;
use anyhow::{Context, Result, bail};
use legaia_code_hooks::arenas::{
    ARENA1_END_VA, ARENA1_VA, ARENA2_END_VA, ARENA2_VA, SCUS_GAP_END_VA, SCUS_GAP_VA,
};
use legaia_code_hooks::mips::{
    self as m, A0, A1, GP, RA, S0, S2, S3, S5, SP, T0, T1, T2, T3, T4, T5, T6, T7, T8, V0, V1, ZERO,
};
use legaia_disc_patch::disc::DiscPatcher;

const SCUS_NAME: &str = "SCUS_942.54";
/// The applier call in the queue builder and the applier it targets.
const APPLIER_VA: u32 = 0x801E_F9E4;
/// `DAT_801F696C`: the applier's own "a Super Art fired" flag.
const SUPER_FIRED_VA: u32 = 0x801F_696C;
const ACTOR_TABLE_VA: u32 = 0x801C_9370;
/// Per-seat record[0] base pointers (the party action tables).
const RECORD0_TABLE_VA: u32 = 0x801C_9360;
const CTX_PTR_VA: u32 = 0x8007_BD24;
/// The MP debit of state `0x28`: `lhu v0, 0x150(s3)`.
const DEBIT_SITE_VA: u32 = 0x801E_4584;
const DEBIT_RETAIL: u32 = 0x9662_0150;
const MODULE_BASE: u32 = 0x801F_69D8;
/// `gp` of the SCUS globals (`gp + 0xA0C` = the battle ctx pointer `0x8007BD24`).
const GP_VA: u32 = 0x8007_B318;

/// First art constant and the number of constants the cast table covers.
pub const TABLE_BASE: u8 = 0x10;
pub const TABLE_LEN: usize = 0x21;
/// The two record[0] rows the stage immediates are redirected to.
pub const STAGE_ROW_A: u8 = 0x0A;
pub const STAGE_ROW_B: u8 = 0x06;

/// Hyper Crush and Hyper Lightning: the spell ids this route knows modules for.
pub const SPELL_HYPER_CRUSH: u8 = 0x4B;
pub const SPELL_HYPER_LIGHTNING: u8 = 0x4C;
/// Cort's Guilty Cross (PROT 944).
pub const SPELL_GUILTY_CROSS: u8 = 0x37;
/// Cort's Mystic Circle (PROT 938, tick body `0x801F69EC`).
pub const SPELL_MYSTIC_CIRCLE: u8 = 0xB7;
/// The row table holds up to `ROW_SLOTS` routed spells, four bytes each:
/// `[spell, 0, pool entry offset (u16)]`, an empty slot has spell `0`.
pub const ROW_SLOTS: usize = 4;

/// One art routed into a cast module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Special {
    /// The art's queue constant (bank row + `0x10`).
    pub constant: u8,
    /// The capture-class spell it casts.
    pub spell: u8,
}

/// The caster stage entries a module asks for, as monster action indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageEntries {
    /// The entry authored on row [`STAGE_ROW_A`], shared by every spell of a
    /// build that stages one there.
    pub row_a: Option<usize>,
    /// The entry authored into the pool the cast prep helper points row
    /// [`STAGE_ROW_B`] at, per spell.
    pub pooled: Option<usize>,
}

/// The stage entries of a spell. Hyper Crush and Hyper Lightning stage a pose
/// on row A and a second one on row B; Guilty Cross stages only row A; Mystic
/// Circle stages one pose, on row B, so it can share a build with Guilty
/// Cross (whose pose owns row A).
pub fn stage_entries(spell: u8) -> Option<StageEntries> {
    let (row_a, pooled) = match spell {
        SPELL_HYPER_CRUSH => (Some(0x0D), Some(0x0B)),
        SPELL_HYPER_LIGHTNING => (Some(0x0D), Some(0x0C)),
        SPELL_GUILTY_CROSS => (Some(0x08), None),
        SPELL_MYSTIC_CIRCLE => (None, Some(0x07)),
        _ => return None,
    };
    Some(StageEntries { row_a, pooled })
}

/// Where everything sits: scratch and tables in ARENA2, the prep helper in
/// ARENA1.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    /// Sweep scratch: `+0` start seat, `+1` end seat (0 = no victims), `+2`
    /// `8 - count`, `+8` `&actor_table[start]`.
    pub scratch: u32,
    /// Constant -> spell table (`0x24` bytes).
    pub table: u32,
    /// Routed spells: `ROW_SLOTS` x `[spell, 0, pool entry offset u16]`
    /// (offset 0 = the spell stages no pooled pose).
    pub rows: u32,
    /// The cast prep helper.
    pub prep: u32,
    /// The readef group base streamed for a routed monster special (one
    /// byte, `0` = none).
    pub readef: u32,
    /// The texture request routine.
    pub hook: u32,
}

pub fn layout() -> Layout {
    Layout {
        scratch: ARENA2_VA,
        table: ARENA2_VA + 0x10,
        rows: ARENA2_VA + 0x34,
        prep: ARENA1_VA,
        readef: ARENA2_VA + 0x44,
        hook: HOOK_VA,
    }
}

/// `gp`-relative displacement of a scratch byte, as the `u16` an I-type
/// instruction carries.
fn gp_off(k: u32) -> u16 {
    ((layout().scratch + k) as i64 - GP_VA as i64) as i16 as u16
}

/// Where the texture request lives: the preserved rodata gap below ARENA1
/// (zero on a retail disc; the bonus-drop family of patcher hooks uses the same
/// run, so this route and those exclude each other).
pub const HOOK_VA: u32 = 0x8007_AB80;
pub const HOOK_END_VA: u32 = 0x8007_AE00;
/// Spells at or above this id are monster specials: no party cast streams a
/// texture bank for them.
const MONSTER_SPELL_MIN: u16 = 0xA1;

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

/// The routed-spell table: every special gets a slot, with its pool entry
/// offset from `pool` (`0` when it has none).
fn row_table(specials: &[Special], pool: &[(u8, usize)]) -> Result<[u8; ROW_SLOTS * 4]> {
    if specials.len() > ROW_SLOTS {
        bail!("at most {ROW_SLOTS} routed spells");
    }
    let mut t = [0u8; ROW_SLOTS * 4];
    for (i, s) in specials.iter().enumerate() {
        let off = pool
            .iter()
            .find(|(sp, _)| *sp == s.spell)
            .map_or(Ok(0), |&(_, off)| {
                u16::try_from(off).context("pool entry offset past 16 bits")
            })?;
        t[i * 4] = s.spell;
        t[i * 4 + 2..i * 4 + 4].copy_from_slice(&off.to_le_bytes());
    }
    Ok(t)
}

fn words_to_bytes(w: Vec<u32>) -> Vec<u8> {
    w.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// The queue hook stub that replaces the `jal` to the Super-Art applier.
/// Seat 0 only (the host); `table_va` holds the constant -> spell table.
pub fn assemble_route(table_va: u32, request_va: Option<u32>) -> Vec<u8> {
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
    if let Some(va) = request_va {
        // the texture request lives in the rodata gap; it returns through
        // its own copy of the epilogue below
        a.op(m::j(va));
        a.op(m::nop());
    }
    a.bind(done);
    a.op(m::lw(RA, SP, 0x14));
    a.op(m::addiu(SP, SP, 0x18));
    a.op(m::jr(RA));
    a.op(m::nop());
    words_to_bytes(a.finish())
}

/// The cast prep helper, called in place of the MP load of state `0x28`'s
/// debit (`s3` = caster, `s5` = `ctx + 0x11`, `s0` = the cost about to be
/// debited). Returns the MP in `v0`, plus the cost for a routed party cast.
/// Clobbers `v1`, `t0 .. t8`.
pub fn assemble_prep(l: &Layout) -> Vec<u8> {
    let mut a = Asm::new();
    let (enemy, write, nz, ret) = (a.label(), a.label(), a.label(), a.label());
    let (scan, found) = (a.label(), a.label());
    a.op(m::lhu(V0, S3, 0x150)); // MP
    a.op(m::lbu(V1, S5, 2)); // caster seat
    a.op(m::lui(T0, m::hi(CTX_PTR_VA)));
    a.op(m::lw(T0, T0, m::lo(CTX_PTR_VA))); // ctx
    a.op(m::nop());
    a.op(m::lbu(T1, T0, 0)); // party count
    a.op(m::lbu(T2, T0, 1)); // monster count
    a.op(m::sltiu(T3, V1, 3)); // party caster?
    a.br(enemy, move |o| m::beq(T3, ZERO, o));
    a.op(m::addu(T4, ZERO, ZERO)); // delay: start = 0
    a.op(m::addiu(T4, ZERO, 3)); // party caster: victims are seats 3..
    a.op(m::addu(T5, T2, T4)); // end = 3 + monsters
    a.br(write, move |o| m::beq(ZERO, ZERO, o));
    a.op(m::nop());
    a.bind(enemy);
    a.op(m::addu(T5, T1, ZERO)); // monster caster: victims are the party
    a.bind(write);
    a.op(m::subu(T6, T5, T4)); // victim count
    a.br(nz, move |o| m::bne(T6, ZERO, o));
    a.op(m::nop());
    a.op(m::addu(T5, ZERO, ZERO)); // none: end = 0
    a.bind(nz);
    a.op(m::addiu(T7, ZERO, 8));
    a.op(m::subu(T7, T7, T6)); // 8 - count
    a.op(m::sb(T4, GP, gp_off(0)));
    a.op(m::sb(T5, GP, gp_off(1)));
    a.op(m::sb(T7, GP, gp_off(2)));
    a.op(m::sll(T8, T4, 2));
    a.op(m::lui(T0, m::hi(ACTOR_TABLE_VA)));
    a.op(m::addiu(T0, T0, m::lo(ACTOR_TABLE_VA)));
    a.op(m::addu(T0, T0, T8));
    a.op(m::sw(T0, GP, gp_off(8))); // &actor_table[start]
    // A routed party Magic cast: MP-neutral, and row 6 points at the pose.
    a.br(ret, move |o| m::beq(T3, ZERO, o));
    a.op(m::lbu(T0, S3, 0x1DE)); // delay: category
    a.op(m::addiu(T1, ZERO, 2)); // (fills the lbu's load delay)
    a.br(ret, move |o| m::bne(T0, T1, o));
    a.op(m::nop());
    a.op(m::lbu(T0, S3, 0x1DF)); // spell
    a.op(m::lui(T1, m::hi(l.rows)));
    a.br(ret, move |o| m::beq(T0, ZERO, o));
    a.op(m::addiu(T1, T1, m::lo(l.rows))); // delay
    a.op(m::addiu(T2, ZERO, ROW_SLOTS as u16));
    a.bind(scan);
    a.op(m::lbu(T4, T1, 0)); // routed spell of this slot
    a.op(m::addiu(T2, T2, 0xFFFF)); // (fills the lbu's load delay)
    a.br(found, move |o| m::beq(T4, T0, o));
    a.op(m::addiu(T1, T1, 4)); // delay: next slot (found reads one back)
    a.br(scan, move |o| m::bne(T2, ZERO, o));
    a.op(m::nop());
    a.br(ret, move |o| m::beq(ZERO, ZERO, o)); // not routed
    a.op(m::nop());
    a.bind(found);
    a.op(m::addu(V0, V0, S0)); // the debit leaves the MP where it was
    a.op(m::lhu(T1, T1, 0xFFFE)); // pool entry offset (slot + 4 - 2)
    a.op(m::nop());
    a.br(ret, move |o| m::beq(T1, ZERO, o)); // no pooled pose
    a.op(m::sll(T0, V1, 2)); // delay
    a.op(m::lui(T2, m::hi(RECORD0_TABLE_VA)));
    a.op(m::addiu(T2, T2, m::lo(RECORD0_TABLE_VA)));
    a.op(m::addu(T0, T0, T2));
    a.op(m::lw(T0, T0, 0)); // record[0] base of the caster's seat
    a.op(m::nop());
    a.op(m::addu(T1, T0, T1)); // pool entry
    a.op(m::sw(T1, T0, 4 * u16::from(STAGE_ROW_B))); // row 6 -> entry
    a.op(m::addiu(T2, T1, 0xAC));
    a.op(m::sw(T2, T1, 0x88)); // entry +0x88: its stream
    a.bind(ret);
    a.op(m::jr(RA));
    a.op(m::nop());
    words_to_bytes(a.finish())
}

/// The texture request of a routed monster special, jumped to from the route
/// stub's hit path (`t3` = the spell id, the stub's frame still on the stack).
/// A monster special's texture page streams with the boss's own readef group
/// (Cort's Mystic Circle rune circle is slot 54 of group 18), but the host's
/// turn streamed its character's group (Vahn's slot 0, the `BACK / READY` menu
/// page). The queue builder runs at action-seed state `0x0C`, after state
/// `0x0B` has waited the streaming applier (`ctx[+0x276]`) idle, so the
/// request is safe to issue there: `ctx[+0x277]` = the group base and
/// `ctx[+0x276] = 1` restart the applier, which streams the group's slots
/// during the cast's own opening. A second builder pass sees the base already
/// seeded and leaves the stream alone.
pub fn assemble_readef_request(l: &Layout) -> Vec<u8> {
    let mut a = Asm::new();
    let out = a.label();
    let readef_off = ((l.readef) as i64 - GP_VA as i64) as i16 as u16;
    a.op(m::sltiu(T4, T3, MONSTER_SPELL_MIN)); // a party spell?
    a.br(out, move |o| m::bne(T4, ZERO, o));
    a.op(m::lbu(T5, GP, readef_off)); // delay: configured group base
    a.op(m::lui(T6, m::hi(CTX_PTR_VA)));
    a.op(m::lw(T6, T6, m::lo(CTX_PTR_VA))); // ctx
    a.br(out, move |o| m::beq(T5, ZERO, o)); // no group configured
    a.op(m::addiu(T0, ZERO, 1)); // delay
    a.op(m::lbu(T7, T6, 0x277)); // the base now seeded
    a.op(m::nop());
    a.br(out, move |o| m::beq(T7, T5, o)); // already requested
    a.op(m::nop());
    a.op(m::sb(T5, T6, 0x277));
    a.op(m::sb(T0, T6, 0x276));
    a.bind(out);
    a.op(m::lw(RA, SP, 0x14)); // the route stub's epilogue
    a.op(m::addiu(SP, SP, 0x18));
    a.op(m::jr(RA));
    a.op(m::nop());
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
pub fn edits_936() -> Vec<ModuleEdit> {
    let lbu_v0_v0 = 0x9042_0000; // lbu v0,0(v0)
    let lbu_v0_s2 = 0x9242_0000; // lbu v0,0(s2)
    let lui_v0_801d = 0x3C02_801D;
    let move_s3_zero = 0x0000_9821; // addu s3,zero,zero
    let move_a0_zero = 0x0000_2021; // addu a0,zero,zero
    let addiu_s2_v0 = 0x2452_9370; // addiu s2,v0,-0x6c90
    let addiu_a1_v0 = 0x2445_9370; // addiu a1,v0,-0x6c90
    // The R3000 reads the OLD value of a register in the instruction right
    // after a load: every pointer load below sits two words before its use.
    let start = m::lbu(S3, GP, gp_off(0));
    let end = m::lbu(V0, GP, gp_off(1));
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
            replace: end,
        },
        ModuleEdit {
            va: 0x801F_7934,
            expect: move_s3_zero,
            replace: start,
        },
        ModuleEdit {
            va: 0x801F_7938,
            expect: lui_v0_801d,
            replace: m::lw(S2, GP, gp_off(8)),
        },
        ModuleEdit {
            va: 0x801F_793C,
            expect: addiu_s2_v0,
            replace: m::nop(),
        },
        ModuleEdit {
            va: 0x801F_7A38,
            expect: lbu_v0_v0,
            replace: end,
        },
        // phase 8 settle wait: victim side, a dead victim is done
        ModuleEdit {
            va: 0x801F_7B18,
            expect: lbu_v0_s2,
            replace: end,
        },
        ModuleEdit {
            va: 0x801F_7B24,
            expect: move_a0_zero,
            replace: m::lbu(A0, GP, gp_off(0)),
        },
        ModuleEdit {
            va: 0x801F_7B30,
            expect: lui_v0_801d,
            replace: m::lw(A1, GP, gp_off(8)),
        },
        ModuleEdit {
            va: 0x801F_7B34,
            expect: addiu_a1_v0,
            replace: m::nop(),
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
        // phase 2 hides a row of actors: the victim side, `count` of them,
        // counted up to 8 so the loop's own bound is one immediate
        ModuleEdit {
            va: 0x801F_6E38,
            expect: 0x0000_2021, // move a0,zero
            replace: m::lw(V1, GP, gp_off(8)),
        },
        ModuleEdit {
            va: 0x801F_6E40,
            expect: 0x00C0_1821, // move v1,a2
            replace: m::lbu(A0, GP, gp_off(2)),
        },
        ModuleEdit {
            va: 0x801F_6E5C,
            expect: 0x2C82_0003, // sltiu v0,a0,3
            replace: m::sltiu(V0, A0, 8),
        },
        // dead victim: no wait for played id 8
        ModuleEdit {
            va: 0x801F_77E4,
            expect: 0x1462_000E, // bne v1,v0,0x801F7820
            replace: m::nop(),
        },
    ]
}

/// PROT 944 (Guilty Cross, `0x37`) edits. Everything in the body goes by
/// seat (the victim off `caster[+0x1DD]`, both rows hidden by their own
/// counts, the caster re-shown by pointer), so only the caster's one stage
/// pose moves: raw row 8 (the Recover row on a party, whose tag byte re-stages
/// itself) becomes row `0x0A`.
pub fn edits_944() -> Vec<ModuleEdit> {
    vec![ModuleEdit {
        va: 0x801F_6B50,
        expect: 0x2402_0008,
        replace: stage_imm(STAGE_ROW_A),
    }]
}

/// PROT 938 (Mystic Circle, `0xB7`) edits. The body is five phases behind the
/// head table: 0 and 1 hide the party row, 2 shows it again, 3 hits it, 4
/// resets the caster. Every loop walks `actor_table[0 .. ctx[+0]]`; a party
/// caster must hide, show and hit the monster row instead, so each loop takes
/// its seat range and pointer from the sweep scratch (see 936). The caster's
/// one stage pose is raw row 7, which becomes row `0x06` (the pooled pose).
pub fn edits_938() -> Vec<ModuleEdit> {
    let ctx = |reg: u32| m::lw(V0, reg, 0xBD24); // lw v0,-0x42dc(reg)
    let load_count = 0x9042_0000; // lbu v0,0(v0)
    let lui_v0_801d = 0x3C02_801D;
    let table_v1 = m::addiu(V1, V0, 0x9370); // addiu v1,v0,-0x6c90
    let end = m::lbu(V0, GP, gp_off(1));
    let ed = |va, expect, replace| ModuleEdit {
        va,
        expect,
        replace,
    };
    // A hide / show loop: the counter starts at the first victim seat, the
    // pointer at its table cell, and the bound is the absolute end seat. The
    // two loads sit two words before the first use of the pointer.
    let sweep = |ctx_reg: u32, entry: u32, setup: u32, tail: u32| {
        vec![
            ed(entry, ctx(ctx_reg), end),
            ed(entry + 8, load_count, m::nop()),
            ed(setup, lui_v0_801d, m::lw(V1, GP, gp_off(8))),
            ed(setup + 4, table_v1, m::lbu(S2, GP, gp_off(0))),
            ed(tail, ctx(ctx_reg), end),
            ed(tail + 8, load_count, m::nop()),
        ]
    };
    let mut v = vec![ed(0x801F_6CC8, 0x2402_0007, stage_imm(STAGE_ROW_B))];
    v.extend(sweep(A0, 0x801F_6BA4, 0x801F_6BBC, 0x801F_6BDC)); // phase 0 hide
    v.extend(sweep(A0, 0x801F_6C04, 0x801F_6C20, 0x801F_6C40)); // phase 1 hide
    v.extend(sweep(A1, 0x801F_6ED8, 0x801F_6EF8, 0x801F_6F18)); // phase 2 show
    // phase 3 damage sweep: the counter is the victim seat
    v.extend([
        ed(0x801F_70A8, ctx(V1), end),
        ed(0x801F_70B0, load_count, m::nop()),
        ed(
            0x801F_70BC,
            m::addu(S2, ZERO, ZERO),
            m::lbu(S2, GP, gp_off(0)),
        ),
        ed(0x801F_70C0, lui_v0_801d, m::lw(S3, GP, gp_off(8))),
        ed(0x801F_70C4, m::addiu(S3, V0, 0x9370), m::nop()),
        ed(0x801F_71C8, ctx(V1), end),
        ed(0x801F_71D0, load_count, m::nop()),
    ]);
    v
}

/// Module PROT entry and edits for a spell.
pub fn module_for(spell: u8) -> Option<(usize, Vec<ModuleEdit>)> {
    match spell {
        SPELL_HYPER_CRUSH => Some((936, edits_936())),
        SPELL_HYPER_LIGHTNING => Some((937, edits_937())),
        SPELL_GUILTY_CROSS => Some((944, edits_944())),
        SPELL_MYSTIC_CIRCLE => Some((938, edits_938())),
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

/// Patch the debit site of state `0x28` in the battle overlay.
fn redirect_debit(patcher: &mut DiscPatcher, prep_va: u32) -> Result<bool> {
    let entry = patcher.read_entry(898)?;
    let at = (DEBIT_SITE_VA - 0x801C_E818) as usize;
    let word = u32::from_le_bytes(entry[at..at + 4].try_into()?);
    let hooked = m::jal(prep_va);
    if word == hooked {
        return Ok(false);
    }
    if word != DEBIT_RETAIL {
        bail!(
            "debit site {DEBIT_SITE_VA:#010x}: expected {DEBIT_RETAIL:#010x}, found {word:#010x}"
        );
    }
    patcher.patch_prot_entry(898, at as u64, &hooked.to_le_bytes())?;
    Ok(true)
}

/// Install the route for `specials`: the queue hook, the cast prep helper and
/// its tables, the module edits and the spell-name banner gate. `pool` maps
/// each spell to the record[0] offset of its second-stage pose. Returns
/// whether anything changed.
pub fn install(
    patcher: &mut DiscPatcher,
    specials: &[Special],
    pool: &[(u8, usize)],
    readef_base: u8,
) -> Result<bool> {
    if specials.is_empty() {
        bail!("module cast route needs at least one special");
    }
    let l = layout();
    let route = assemble_route(l.table, (readef_base != 0).then_some(l.hook));
    if SCUS_GAP_VA + route.len() as u32 > SCUS_GAP_END_VA {
        bail!("module-cast route stub does not fit the SCUS gap");
    }
    let prep = assemble_prep(&l);
    if l.prep + prep.len() as u32 > ARENA1_END_VA {
        bail!("module-cast prep helper does not fit ARENA1");
    }
    if l.readef + 1 > ARENA2_END_VA || l.rows + (ROW_SLOTS * 4) as u32 > l.readef {
        bail!("module-cast tables do not fit ARENA2");
    }
    let mut changed = false;
    changed |= write_scus(patcher, SCUS_GAP_VA, &route)?;
    changed |= write_scus(patcher, l.prep, &prep)?;
    changed |= write_scus(patcher, l.table, &table(specials))?;
    changed |= write_scus(patcher, l.rows, &row_table(specials, pool)?)?;
    changed |= redirect_debit(patcher, l.prep)?;
    if readef_base != 0 {
        let hook = assemble_readef_request(&l);
        if l.hook + hook.len() as u32 > HOOK_END_VA {
            bail!("readef request does not fit its gap");
        }
        changed |= write_scus(patcher, l.hook, &hook)?;
        changed |= write_scus(patcher, l.readef, &[readef_base])?;
    }
    changed |= legaia_delilas_party::delilas_cast::redirect_applier_call(patcher, SCUS_GAP_VA)?;
    changed |= legaia_delilas_party::delilas_cast::install_cast_label_gate(patcher)?;
    let mut done = Vec::new();
    for s in specials {
        let (prot, edits) = module_for(s.spell)
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
    const REC0: u32 = 0x8012_0000;
    const RETURN_VA: u32 = 0x801E_FA00;
    const STACK: u32 = 0x801F_F000;
    const SPECIALS: [Special; 3] = [
        Special {
            constant: 0x27,
            spell: 0x4B,
        },
        Special {
            constant: 0x29,
            spell: 0x4C,
        },
        Special {
            constant: 0x2B,
            spell: 0x37,
        },
    ];

    /// Run the route stub. Returns `(category, queue head)`.
    fn route(char_idx: u32, super_fired: u32, queue: &[u8]) -> (u8, u8) {
        let mut cpu = Cpu::new();
        let l = layout();
        cpu.load(SCUS_GAP_VA, &assemble_route(l.table, None));
        cpu.load(l.table, &table(&SPECIALS));
        // the applier: returns at once
        cpu.load_words(APPLIER_VA, &[m::jr(RA), m::nop()]);
        cpu.wr32(ACTOR_TABLE_VA, ACTOR);
        cpu.wr32(SUPER_FIRED_VA, super_fired);
        cpu.wr8(ACTOR + 0x1DE, 3);
        cpu.load(ACTOR + 0x1DF, queue);
        cpu.r[A0 as usize] = 0; // slot
        cpu.r[A1 as usize] = char_idx;
        cpu.r[SP as usize] = STACK;
        cpu.r[RA as usize] = RETURN_VA;
        cpu.pc = SCUS_GAP_VA;
        cpu.run_until(&[RETURN_VA]);
        assert_eq!(cpu.r[SP as usize], STACK, "stack balanced");
        (cpu.rd8(ACTOR + 0x1DE), cpu.rd8(ACTOR + 0x1DF))
    }

    #[test]
    fn a_cast_art_becomes_the_spell() {
        assert_eq!(route(0, 0, &[0x0F, 0x0E, 0x19, 0x27, 0]), (2, 0x4B));
        assert_eq!(route(0, 0, &[0x0D, 0x1A, 0x29, 0]), (2, 0x4C));
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
            assert_eq!(route(ch, sup, q), (3, q[0]), "{ch} {sup} {q:x?}");
        }
    }

    struct Run {
        cpu: Cpu,
    }

    /// Run the prep helper for a caster at `seat` casting `spell` with
    /// category `cat`, MP `mp`, debit `cost`.
    fn prep(seat: u8, cat: u8, spell: u8, mp: u16, cost: u32, party: u8, monsters: u8) -> Run {
        let mut cpu = Cpu::new();
        let l = layout();
        cpu.load(l.prep, &assemble_prep(&l));
        cpu.load(
            l.rows,
            &row_table(&SPECIALS, &[(0x4B, 0x1234), (0x4C, 0x2468)]).unwrap(),
        );
        cpu.wr32(CTX_PTR_VA, CTX);
        cpu.wr8(CTX, party);
        cpu.wr8(CTX + 1, monsters);
        cpu.wr8(CTX + 0x13, seat);
        cpu.wr8(ACTOR + 0x1DE, cat);
        cpu.wr8(ACTOR + 0x1DF, spell);
        cpu.wr16(ACTOR + 0x150, mp);
        for s in 0..3u32 {
            cpu.wr32(RECORD0_TABLE_VA + s * 4, REC0 + s * 0x1_0000);
        }
        cpu.r[S3 as usize] = ACTOR;
        cpu.r[S5 as usize] = CTX + 0x11;
        cpu.r[S0 as usize] = cost;
        cpu.r[GP as usize] = GP_VA;
        cpu.r[RA as usize] = RETURN_VA;
        cpu.pc = l.prep;
        cpu.run_until(&[RETURN_VA]);
        Run { cpu }
    }

    impl Run {
        fn mp(&self) -> u32 {
            self.cpu.r[V0 as usize]
        }
        fn scratch(&self, k: u32) -> u8 {
            self.cpu.rd8(layout().scratch + k)
        }
        fn word(&self, k: u32) -> u32 {
            self.cpu.rd32(layout().scratch + k)
        }
    }

    #[test]
    fn a_party_cast_writes_the_monster_side_and_stays_mp_neutral() {
        let r = prep(0, 2, 0x4B, 42, 180, 3, 2);
        assert_eq!(r.mp(), 42 + 180, "the debit of 180 leaves 42");
        assert_eq!((r.scratch(0), r.scratch(1), r.scratch(2)), (3, 5, 6));
        assert_eq!(r.word(8), ACTOR_TABLE_VA + 12);
        // row 6 -> pool entry of the spell, its stream pointer rebuilt
        let entry = REC0 + 0x1234;
        assert_eq!(r.cpu.rd32(REC0 + 6 * 4), entry);
        assert_eq!(r.cpu.rd32(entry + 0x88), entry + 0xAC);
        let r = prep(2, 2, 0x4C, 10, 120, 3, 4);
        assert_eq!(r.mp(), 130);
        let base = REC0 + 2 * 0x1_0000;
        assert_eq!(r.cpu.rd32(base + 6 * 4), base + 0x2468);
        assert_eq!((r.scratch(0), r.scratch(1), r.scratch(2)), (3, 7, 4));
    }

    #[test]
    fn a_monster_cast_gets_the_party_row_and_nothing_else() {
        let r = prep(3, 2, 0x4B, 800, 180, 3, 2);
        assert_eq!(r.mp(), 800, "retail debit untouched");
        assert_eq!((r.scratch(0), r.scratch(1), r.scratch(2)), (0, 3, 5));
        assert_eq!(r.word(8), ACTOR_TABLE_VA);
        assert_eq!(r.cpu.rd32(REC0 + 6 * 4), 0, "no row repoint");
    }

    #[test]
    fn other_party_casts_are_untouched() {
        for (cat, spell) in [(2u8, 0x81u8), (2, 0x4F), (1, 0x4B), (2, 0x4A)] {
            let r = prep(0, cat, spell, 77, 10, 3, 2);
            assert_eq!(r.mp(), 77, "{cat} {spell:#x}");
            assert_eq!(r.cpu.rd32(REC0 + 6 * 4), 0);
        }
        // a spell in the row table's range that nothing routes is left alone
        let r = prep(0, 2, 0x4D, 77, 10, 3, 2);
        assert_eq!((r.mp(), r.cpu.rd32(REC0 + 6 * 4)), (77, 0));
    }

    #[test]
    fn a_spell_without_a_pooled_pose_is_still_mp_neutral() {
        let r = prep(0, 2, 0x37, 42, 60, 3, 2);
        assert_eq!(r.mp(), 42 + 60);
        assert_eq!(r.cpu.rd32(REC0 + 6 * 4), 0, "no row repoint");
        assert_eq!((r.scratch(0), r.scratch(1)), (3, 5));
    }

    #[test]
    fn no_victims_means_end_zero() {
        let r = prep(0, 2, 0x4B, 1, 1, 3, 0);
        assert_eq!((r.scratch(1), r.scratch(2)), (0, 8));
    }

    /// Run the texture request for a hit on `spell`, with `configured` as the
    /// readef base byte and `seeded` as the base already in `ctx[+0x277]`.
    /// Returns `(ctx[+0x277], ctx[+0x276])`.
    fn readef_request(spell: u8, configured: u8, seeded: u8) -> (u8, u8) {
        let mut cpu = Cpu::new();
        let l = layout();
        cpu.load(l.hook, &assemble_readef_request(&l));
        cpu.wr8(l.readef, configured);
        cpu.wr32(CTX_PTR_VA, CTX);
        cpu.wr8(CTX + 0x277, seeded);
        cpu.wr8(CTX + 0x276, 0);
        cpu.wr32(STACK + 0x14, RETURN_VA); // the stub's saved ra
        cpu.r[SP as usize] = STACK;
        cpu.r[T3 as usize] = u32::from(spell);
        cpu.r[GP as usize] = GP_VA;
        cpu.pc = l.hook;
        cpu.run_until(&[RETURN_VA]);
        assert_eq!(cpu.r[SP as usize], STACK + 0x18, "stub frame popped");
        (cpu.rd8(CTX + 0x277), cpu.rd8(CTX + 0x276))
    }

    #[test]
    fn a_routed_monster_special_requests_the_boss_group() {
        assert_eq!(readef_request(0xB7, 0x36, 0), (0x36, 1));
        assert_eq!(readef_request(0xAD, 0x36, 0), (0x36, 1));
    }

    #[test]
    fn a_second_builder_pass_leaves_the_stream_alone() {
        assert_eq!(readef_request(0xB7, 0x36, 0x36), (0x36, 0));
    }

    #[test]
    fn party_spells_and_unconfigured_builds_request_nothing() {
        assert_eq!(readef_request(0x37, 0x36, 0), (0, 0), "Guilty Cross");
        assert_eq!(readef_request(0x4B, 0x36, 0), (0, 0), "Hyper Crush");
        assert_eq!(readef_request(0xB7, 0, 0), (0, 0), "no group configured");
    }

    fn words(b: &[u8]) -> Vec<u32> {
        b.chunks(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect()
    }

    #[test]
    fn no_stub_reads_a_register_in_its_load_delay() {
        let l = layout();
        assert!(crate::hazard::load_hazards(&words(&assemble_route(l.table, None))).is_empty());
        assert!(crate::hazard::load_hazards(&words(&assemble_prep(&l))).is_empty());
        assert!(crate::hazard::load_hazards(&words(&assemble_readef_request(&l))).is_empty());
    }

    #[test]
    fn everything_fits() {
        let l = layout();
        assert!(l.prep as usize + assemble_prep(&l).len() <= ARENA1_END_VA as usize);
        assert!(l.rows as usize + ROW_SLOTS * 4 <= l.readef as usize);
        assert!(l.readef < ARENA2_END_VA);
        assert!(l.hook as usize + assemble_readef_request(&l).len() <= HOOK_END_VA as usize);
        assert!(
            SCUS_GAP_VA as usize + assemble_route(l.table, Some(l.hook)).len()
                <= SCUS_GAP_END_VA as usize
        );
        // gp-relative scratch reaches: 0x8007AFF8 - 0x8007B318 = -0x320
        assert_eq!(gp_off(0), 0xFCE0);
    }
}
