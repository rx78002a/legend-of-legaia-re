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
    self as m, A0, A1, A2, A3, GP, RA, S0, S1, S2, S3, S5, SP, T0, T1, T2, T3, T4, T5, T6, T7, T8,
    V0, V1, ZERO,
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
/// Cort's Mystic Shield (PROT 940, tick body `0x801F7240`).
pub const SPELL_MYSTIC_SHIELD: u8 = 0xAC;
/// Cort's Evil Seru Magic (PROT 966, tick body `0x801F6A74`).
pub const SPELL_EVIL_SERU: u8 = 0xAD;
/// Songi's third form: Chaos Flare (PROT 951, tick body `0x801F6A20`).
pub const SPELL_CHAOS_FLARE: u8 = 0x36;
/// Songi's third form: Genocidal Cannon (PROT 963, tick body `0x801F6A20`).
pub const SPELL_GENOCIDAL_CANNON: u8 = 0xB3;
/// Cort Juggernaut's Final Crisis (PROT 961, tick body `0x801F69D8`; the same
/// body serves Dead End Crisis `0xA1`).
pub const SPELL_FINAL_CRISIS: u8 = 0xB4;

/// Specials whose effects draw from the texture bank of the boss's own readef
/// group (Cort's rune circle, the Juggernaut and the shield; Songi's flame):
/// a cast of one asks for that group (see [`assemble_readef_request`]).
pub fn needs_group(spell: u8) -> bool {
    matches!(
        spell,
        SPELL_MYSTIC_CIRCLE
            | SPELL_MYSTIC_SHIELD
            | SPELL_EVIL_SERU
            | SPELL_CHAOS_FLARE
            | SPELL_GENOCIDAL_CANNON
            | SPELL_FINAL_CRISIS
    )
}

/// The art each spell is routed onto by default (`--special` overrides):
/// Somersault, Hyper Elbow and Slash Kick.
pub fn default_constant(spell: u8) -> Option<u8> {
    match spell {
        SPELL_HYPER_CRUSH | SPELL_GUILTY_CROSS | SPELL_CHAOS_FLARE | SPELL_FINAL_CRISIS => {
            Some(0x27)
        }
        SPELL_HYPER_LIGHTNING | SPELL_MYSTIC_CIRCLE | SPELL_GENOCIDAL_CANNON => Some(0x29),
        SPELL_MYSTIC_SHIELD => Some(0x26),
        SPELL_EVIL_SERU => Some(0x28),
        _ => None,
    }
}
/// The row table holds up to `ROW_SLOTS` routed spells, four bytes each:
/// `[spell, needs the boss's texture group, pool entry offset (u16)]`, an
/// empty slot has spell `0`.
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
        // Chaos Flare stages raw row 8 too (Songi's third form owns its cast
        // clip at entry 8, like Cort's Guilty Cross)
        SPELL_CHAOS_FLARE | SPELL_GENOCIDAL_CANNON => (Some(0x08), None),
        SPELL_MYSTIC_CIRCLE => (None, Some(0x07)),
        // the shield body stages no caster pose: the caster holds its idle
        SPELL_MYSTIC_SHIELD => (None, None),
        // Evil Seru Magic stages raw row 6 once (the pooled row) and row 0
        SPELL_EVIL_SERU => (None, Some(0x06)),
        // Final Crisis stages raw row 6 once (the Juggernaut's first cast clip)
        SPELL_FINAL_CRISIS => (None, Some(0x06)),
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
    /// Mystic Shield: the damage finisher's side check, the effect-handle
    /// store, the caster-seat loader and the per-frame pass's seat loader.
    pub shield_finisher: u32,
    pub shield_store: u32,
    pub shield_caster: u32,
    pub shield_pass: u32,
    /// The shield owner's seat (one byte; ARENA2).
    pub owner: u32,
    /// Mystic Circle's phase-0 guard (waits for the texture stream).
    pub guard_938: u32,
    /// Evil Seru Magic's phase-0 guard.
    pub guard_966: u32,
    /// Chaos Flare's voice guard.
    pub guard_951: u32,
    /// Songi's third form (Chaos Flare, Genocidal Cannon): the loader of the
    /// caster's stage entry (see [`assemble_cf_entry`]), the caster-cell
    /// loader (see [`assemble_caster_cell`]) and Genocidal Cannon's voice
    /// guard. These share the rodata run with the Mystic Shield routines, which
    /// a Songi build never installs (an install over a used run refuses).
    pub cf_entry: u32,
    pub caster_cell: u32,
    pub guard_963: u32,
    /// Final Crisis's voice guard (shares the run with Chaos Flare's, which a
    /// Juggernaut build never installs).
    pub guard_961: u32,
    /// Evil Seru Magic's stand-in record[0] table (see [`assemble_esm_dummy`]).
    pub esm_dummy: u32,
}

pub fn layout() -> Layout {
    Layout {
        scratch: ARENA2_VA,
        table: ARENA2_VA + 0x10,
        rows: ARENA2_VA + 0x34,
        prep: ARENA1_VA,
        readef: ARENA2_VA + 0x44,
        hook: HOOK_VA,
        shield_finisher: HOOK_VA + 0x80,
        shield_store: HOOK_VA + 0x104,
        shield_caster: HOOK_VA + 0x124,
        shield_pass: HOOK_VA + 0x148,
        owner: ARENA2_VA + 0x45,
        guard_938: HOOK_VA + 0x168,
        guard_966: HOOK_VA + 0x19C,
        guard_951: HOOK_VA + 0x80,
        guard_963: HOOK_VA + 0xB4,
        guard_961: HOOK_VA + 0x80,
        cf_entry: HOOK_VA + 0xE8,
        caster_cell: HOOK_VA + 0x118,
        esm_dummy: HOOK_VA + 0x208,
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

/// The routed-spell table: every special gets a slot, with whether it needs
/// the boss's texture group and its pool entry offset from `pool` (`0` when it
/// has none).
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
        t[i * 4 + 1] = u8::from(needs_group(s.spell));
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
    a.op(m::sb(T6, GP, gp_off(3))); // victim count
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
/// A monster special's effects draw from the texture bank of the boss's own
/// readef group (Cort's rune circle is slot 54 of group 18), but the host's
/// turn streamed its character's group (Vahn's slot 0, the `BACK / READY` menu
/// page). The queue builder runs at action-seed state `0x0C`, after state
/// `0x0B` has waited the streaming applier (`ctx[+0x276]`) idle, so the
/// request is safe to issue there: `ctx[+0x277]` = the group base and
/// `ctx[+0x276] = 1` restart the applier, which streams the group's slots
/// during the cast's own opening. Only a spell whose row says it needs the
/// group asks; a second builder pass sees the base already seeded and leaves
/// the stream alone.
pub fn assemble_readef_request(l: &Layout) -> Vec<u8> {
    let mut a = Asm::new();
    let (out, scan, found) = (a.label(), a.label(), a.label());
    let readef_off = ((l.readef) as i64 - GP_VA as i64) as i16 as u16;
    a.op(m::lui(T1, m::hi(l.rows)));
    a.op(m::addiu(T1, T1, m::lo(l.rows)));
    a.op(m::addiu(T2, ZERO, ROW_SLOTS as u16));
    a.bind(scan);
    a.op(m::lbu(T4, T1, 0)); // routed spell of this slot
    a.op(m::addiu(T2, T2, 0xFFFF)); // (fills the lbu's load delay)
    a.br(found, move |o| m::beq(T4, T3, o));
    a.op(m::nop());
    a.br(scan, move |o| m::bne(T2, ZERO, o));
    a.op(m::addiu(T1, T1, 4)); // delay: next slot
    a.br(out, move |o| m::beq(ZERO, ZERO, o)); // not routed
    a.op(m::nop());
    a.bind(found);
    a.op(m::lbu(T4, T1, 1)); // does the spell need the group?
    a.op(m::lbu(T5, GP, readef_off)); // configured group base
    a.op(m::nop());
    a.br(out, move |o| m::beq(T4, ZERO, o));
    a.op(m::nop());
    a.br(out, move |o| m::beq(T5, ZERO, o)); // no group configured
    a.op(m::lui(T6, m::hi(CTX_PTR_VA))); // delay
    a.op(m::lw(T6, T6, m::lo(CTX_PTR_VA))); // ctx
    a.op(m::addiu(T0, ZERO, 1));
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

/// Where Mystic Shield's retail code is patched.
const SHIELD_FINISHER_SITE_VA: u32 = 0x801D_DB90; // bnez v1,0x801DDBCC
const SHIELD_FINISHER_PARTY_VA: u32 = 0x801D_DBCC;
const SHIELD_FINISHER_DONE_VA: u32 = 0x801D_E094;
const SHIELD_PASS_SITE_VA: u32 = 0x8004_D55C; // lw v0,0xc(a1)
const SHIELD_STORE_VA: u32 = 0x801F_7678; // sw v0,-0x427c(v1)
const SHIELD_CASTER_VA: u32 = 0x801F_7648; // lui s0,0x801d
/// The shield's handle word (`-0x427C` from `0x80080000`).
const SHIELD_HANDLE_OFF: u16 = 0xBD84;

fn owner_off(l: &Layout) -> u16 {
    (l.owner as i64 - GP_VA as i64) as i16 as u16
}

/// `FUN_801DDB30`, the damage finisher, halves every hit on an **enemy**
/// defender while the shield's handle word is live. The shield belongs to
/// whoever cast it, so the host's shield halves hits on the party instead:
/// the defender's side must equal the owner's side. Entered through a `j`
/// that replaces `bnez v1,<party path>` (`v1` = defender is a party seat;
/// its delay slot `sltiu v0,a2,3` is the original and stays).
pub fn assemble_shield_finisher(l: &Layout) -> Vec<u8> {
    let mut a = Asm::new();
    let (plain, party) = (a.label(), a.label());
    a.op(m::lui(T0, 0x8008));
    a.op(m::lw(T0, T0, SHIELD_HANDLE_OFF)); // the shield's handle
    a.op(m::lbu(T1, GP, owner_off(l))); // the owner's seat
    a.op(m::nop());
    a.br(plain, move |o| m::beq(T0, ZERO, o)); // no shield up
    a.op(m::sltiu(T1, T1, 3)); // delay: owner is a party seat
    a.br(plain, move |o| m::bne(V1, T1, o)); // the other side's shield
    a.op(m::nop());
    a.op(m::lw(T2, S0, 0)); // damage
    a.op(m::lw(T3, S2, 0)); // its floor
    a.op(m::nop());
    a.op(m::subu(T2, T2, T3));
    a.op(m::srl(T2, T2, 1));
    a.op(m::addu(T3, T3, T2));
    a.op(m::sw(T3, S0, 0));
    a.bind(plain);
    a.br(party, move |o| m::bne(V1, ZERO, o)); // a party defender: its own path
    a.op(m::nop());
    a.op(m::j(SHIELD_FINISHER_DONE_VA)); // an enemy defender: nothing more
    a.op(m::nop());
    a.bind(party);
    a.op(m::j(SHIELD_FINISHER_PARTY_VA));
    a.op(m::nop());
    words_to_bytes(a.finish())
}

/// Stores the shield's effect handle (`v0`) and the caster's seat as its owner;
/// entered by a `jal` at the module's `sw v0,-0x427c(v1)` (`v1` = `0x80080000`,
/// the delay slot is the next original word).
pub fn assemble_shield_store(l: &Layout) -> Vec<u8> {
    let mut a = Asm::new();
    a.op(m::sw(V0, V1, SHIELD_HANDLE_OFF));
    a.op(m::lw(T0, V1, 0xBD24)); // ctx
    a.op(m::nop());
    a.op(m::lbu(T0, T0, 0x13)); // caster seat
    a.op(m::nop());
    a.op(m::sb(T0, GP, owner_off(l)));
    a.op(m::jr(RA));
    a.op(m::nop());
    words_to_bytes(a.finish())
}

/// The module's `lui s0,0x801d` (+ `addiu s0,s0,-0x6c90`) made `s0` the actor
/// table so the following `0xC(s0)` reads seat 3, the first monster. This makes
/// `s0` the table cell *before* the caster's, so those reads hit the caster.
pub fn assemble_shield_caster() -> Vec<u8> {
    let mut a = Asm::new();
    a.op(m::lui(T0, 0x8008));
    a.op(m::lw(T0, T0, 0xBD24)); // ctx
    a.op(m::lui(S0, 0x801D));
    a.op(m::lbu(T0, T0, 0x13)); // caster seat
    a.op(m::addiu(S0, S0, m::lo(ACTOR_TABLE_VA - 0xC)));
    a.op(m::sll(T0, T0, 2));
    a.op(m::addu(S0, S0, T0));
    a.op(m::jr(RA));
    a.op(m::nop());
    words_to_bytes(a.finish())
}

/// The per-frame pass that keeps the shield on its owner (`FUN_8004CE2C`,
/// SCUS) reads the owner as seat 3 through `a1` = actor table. Entered by a
/// `jal` that replaces its first `lw v0,0xC(a1)` (the delay slot is the `nop`
/// after it): `a1` becomes the cell before the owner's, so every `0xC(a1)` of
/// the pass hits the owner, whose half-HP test then breaks the shield.
pub fn assemble_shield_pass(l: &Layout) -> Vec<u8> {
    let mut a = Asm::new();
    a.op(m::lbu(T1, GP, owner_off(l)));
    a.op(m::lui(T2, m::hi(ACTOR_TABLE_VA - 0xC)));
    a.op(m::addiu(T2, T2, m::lo(ACTOR_TABLE_VA - 0xC)));
    a.op(m::sll(T1, T1, 2));
    a.op(m::addu(A1, T2, T1));
    a.op(m::lw(V0, A1, 0xC));
    a.op(m::jr(RA));
    a.op(m::nop());
    words_to_bytes(a.finish())
}

/// PROT 940's Mystic Shield body: the shield effect and the blanked reaction
/// run follow the caster instead of seat 3, and the owner is recorded.
pub fn edits_940() -> Vec<ModuleEdit> {
    let l = layout();
    vec![
        ModuleEdit {
            va: SHIELD_CASTER_VA,
            expect: m::lui(S0, 0x801D),
            replace: m::jal(l.shield_caster),
        },
        ModuleEdit {
            va: SHIELD_STORE_VA,
            expect: m::sw(V0, V1, SHIELD_HANDLE_OFF),
            replace: m::jal(l.shield_store),
        },
    ]
}

/// The voice cue call every cast module opens with (`jal FUN_8004FCC8(cue)`).
const CUE_FN_VA: u32 = 0x8004_FCC8;
const MC_CUE_SITE_VA: u32 = 0x801F_6AA4;
const MC_CUE_RESUME_VA: u32 = 0x801F_6AAC;
const MC_EPILOGUE_VA: u32 = 0x801F_7240;
const MC_CUE_ID: u16 = 0x1AC;
const ESM_CUE_SITE_VA: u32 = 0x801F_6B1C;
const ESM_CUE_RESUME_VA: u32 = 0x801F_6B24;
const ESM_EPILOGUE_VA: u32 = 0x801F_8D34;
const ESM_CUE_ID: u16 = 0x1AE;
const CF_CUE_SITE_VA: u32 = 0x801F_6C60;
const CF_CUE_RESUME_VA: u32 = 0x801F_6C68;
const CF_EPILOGUE_VA: u32 = 0x801F_77B4;
const CF_CUE_ID: u16 = 0x1B5;
const GC_CUE_SITE_VA: u32 = 0x801F_6AD4;
const GC_CUE_RESUME_VA: u32 = 0x801F_6ADC;
const GC_EPILOGUE_VA: u32 = 0x801F_8178;
const GC_CUE_ID: u16 = 0x1B4;
const FC_CUE_SITE_VA: u32 = 0x801F_6AD4;
const FC_CUE_RESUME_VA: u32 = 0x801F_6ADC;
const FC_EPILOGUE_VA: u32 = 0x801F_7870;
/// The cue Final Crisis plays in the formation it is authored for (the evolved
/// Cort's, `0xB5`); every formation-dependent read of the module is forced to
/// that branch (see [`edits_961`]).
const FC_CUE_ID: u16 = 0x1C1;

/// A cast whose texture group was requested from the route stub starts while
/// the stream is still reading the CD, and the opening voice cue (CD-XA on the
/// same drive) dies with it. A module's phase 0 begins with that cue, so it
/// waits (the tick returns "busy" and phase 0 runs again next frame) until the
/// streaming applier (`ctx[+0x276]`) is idle. Entered by a `j` that replaces
/// the cue's `jal` (the delay slot is the original `addiu a0,zero,cue`).
pub fn assemble_cue_guard(cue: u16, resume: u32, exit: u32) -> Vec<u8> {
    let mut a = Asm::new();
    let wait = a.label();
    a.op(m::lui(T0, 0x8008));
    a.op(m::lw(T0, T0, 0xBD24)); // ctx
    a.op(m::nop());
    a.op(m::lbu(T0, T0, 0x276)); // applier stage
    a.op(m::nop());
    a.br(wait, move |o| m::bne(T0, ZERO, o));
    a.op(m::nop());
    a.op(m::jal(CUE_FN_VA));
    a.op(m::addiu(A0, ZERO, cue));
    a.op(m::j(resume));
    a.op(m::nop());
    a.bind(wait);
    a.op(m::j(exit));
    a.op(m::nop());
    words_to_bytes(a.finish())
}

pub fn assemble_mc_guard() -> Vec<u8> {
    assemble_cue_guard(MC_CUE_ID, MC_CUE_RESUME_VA, MC_EPILOGUE_VA)
}

pub fn assemble_cf_guard() -> Vec<u8> {
    assemble_cue_guard(CF_CUE_ID, CF_CUE_RESUME_VA, CF_EPILOGUE_VA)
}

pub fn assemble_fc_guard() -> Vec<u8> {
    assemble_cue_guard(FC_CUE_ID, FC_CUE_RESUME_VA, FC_EPILOGUE_VA)
}

pub fn assemble_gc_guard() -> Vec<u8> {
    assemble_cue_guard(GC_CUE_ID, GC_CUE_RESUME_VA, GC_EPILOGUE_VA)
}

pub fn assemble_esm_guard() -> Vec<u8> {
    assemble_cue_guard(ESM_CUE_ID, ESM_CUE_RESUME_VA, ESM_EPILOGUE_VA)
}

/// Evil Seru Magic's arm 0 shrinks the victims and halves a field two
/// pointers deep in each seat's record[0] (`*(0x801C9360 + seat * 4) -> +0x14
/// -> +0xE`), and arm 28 puts it back. Monster seats have no such record[0],
/// so the loops read this stand-in table instead: eight pointers to one
/// record whose `+0x14` points at a scratch halfword.
pub fn assemble_esm_dummy(l: &Layout) -> Vec<u8> {
    let table = l.esm_dummy;
    let record = table + 0x20;
    let scratch = table + 0x38;
    let mut b = vec![0u8; 0x48];
    for i in 0..8 {
        b[i * 4..i * 4 + 4].copy_from_slice(&record.to_le_bytes());
    }
    b[0x20 + 0x14..0x20 + 0x18].copy_from_slice(&scratch.to_le_bytes());
    b
}

/// Chaos Flare retimes the caster's cast clip while it plays: it reaches the
/// clip's entry through `*(0x801C9348) + 0x6C` (the first monster's record,
/// entry 8) and writes its hold window (`+0x85` / `+0x86`): frames 12..17
/// while the caster charges (arms crossed), then 21..23 for the release. On a
/// party caster that chain leads into the *enemy's* record and the caster's
/// own pose keeps its authored window. This returns, in `v1`, the entry
/// pointer the caster's row `0x0A` holds, for a `jal` that replaces each
/// `lw v0,-0x6CB8(a0)` (its delay slot is the `nop` after it, and the
/// `lw v1,0x6C(v0)` that follows becomes a `nop`).
pub fn assemble_cf_entry() -> Vec<u8> {
    let mut a = Asm::new();
    a.op(m::lui(T0, 0x8008));
    a.op(m::lw(T0, T0, 0xBD24)); // ctx
    a.op(m::lui(T1, m::hi(RECORD0_TABLE_VA)));
    a.op(m::lbu(T0, T0, 0x13)); // caster seat
    a.op(m::addiu(T1, T1, m::lo(RECORD0_TABLE_VA)));
    a.op(m::sll(T0, T0, 2));
    a.op(m::addu(T1, T1, T0));
    a.op(m::lw(T1, T1, 0)); // record[0] base of the seat
    a.op(m::nop());
    a.op(m::lw(V1, T1, 4 * u16::from(STAGE_ROW_A))); // the row-A entry
    a.op(m::jr(RA));
    a.op(m::nop());
    words_to_bytes(a.finish())
}

/// Genocidal Cannon's effect stager (`0x801F81A0`, run by the effect VM) seats
/// its flares on `actor_table[3]`, the first monster: `addiu v1,v1,-0x6C90`
/// then `lw v0,0xC(v1)` (twice, once per arm). This makes `v1` the table cell
/// *before* the caster's and loads `v0` = the caster, for a `jal` that
/// replaces the `addiu` (the delay slot is the original `lw`, harmless on the
/// stale `v1`, and the return skips it).
pub fn assemble_caster_cell() -> Vec<u8> {
    let mut a = Asm::new();
    a.op(m::lui(T0, 0x8008));
    a.op(m::lw(T0, T0, 0xBD24)); // ctx
    a.op(m::lui(T1, m::hi(ACTOR_TABLE_VA - 0xC)));
    a.op(m::lbu(T0, T0, 0x13)); // caster seat
    a.op(m::addiu(T1, T1, m::lo(ACTOR_TABLE_VA - 0xC)));
    a.op(m::sll(T0, T0, 2));
    a.op(m::addu(V1, T1, T0));
    a.op(m::lw(V0, V1, 0xC)); // the caster
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
    let mut v = vec![
        ed(0x801F_6CC8, 0x2402_0007, stage_imm(STAGE_ROW_B)),
        ed(MC_CUE_SITE_VA, m::jal(CUE_FN_VA), m::j(layout().guard_938)),
    ];
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

/// How a word of PROT 966's Evil Seru Magic body is rewritten.
#[derive(Debug, Clone, Copy)]
enum Site {
    /// `lbu rX,0(ctx)` (the party count) becomes the victim count.
    LbuCount(u32),
    /// The same, for a loop whose counter is the victim *seat*: the end seat.
    LbuEnd(u32),
    /// `move rX,zero` before such a loop becomes the first victim seat.
    LbuStart(u32),
    /// `lui` of the actor-table base becomes the load of the first victim's
    /// table cell (the `addiu` after it becomes a `nop`).
    LwBase(u32),
    Nop,
    /// The two words that build the record[0] table base become the stand-in.
    Rec0Lui(u32),
    Rec0Addiu(u32),
}

const ESM_SITES: &[(u32, u32, Site)] = &[
    (0x801F6B9C, 0x90620000, Site::LbuCount(V0)),
    (0x801F6BB8, 0x3C02801D, Site::Rec0Lui(V0)),
    (0x801F6BBC, 0x24469360, Site::Rec0Addiu(A2)),
    (0x801F6BC8, 0x3C02801D, Site::LwBase(A1)),
    (0x801F6BCC, 0x24459370, Site::Nop),
    (0x801F6C1C, 0x90630000, Site::LbuCount(V1)),
    (0x801F6D10, 0x90420000, Site::LbuCount(V0)),
    (0x801F6D68, 0x90420000, Site::LbuCount(V0)),
    (0x801F6D7C, 0x3C02801D, Site::LwBase(V1)),
    (0x801F6D80, 0x24439370, Site::Nop),
    (0x801F6DA4, 0x90420000, Site::LbuCount(V0)),
    (0x801F72E4, 0x90420000, Site::LbuCount(V0)),
    (0x801F7300, 0x3C02801D, Site::LwBase(V1)),
    (0x801F7304, 0x24439370, Site::Nop),
    (0x801F7328, 0x90420000, Site::LbuCount(V0)),
    (0x801F73F0, 0x90420000, Site::LbuCount(V0)),
    (0x801F7410, 0x3C02801D, Site::LwBase(A2)),
    (0x801F7414, 0x24469370, Site::Nop),
    (0x801F74FC, 0x90420000, Site::LbuCount(V0)),
    (0x801F75D4, 0x90420000, Site::LbuCount(V0)),
    (0x801F75F4, 0x3C02801D, Site::LwBase(A2)),
    (0x801F75F8, 0x24469370, Site::Nop),
    (0x801F76E0, 0x90420000, Site::LbuCount(V0)),
    (0x801F77A4, 0x90420000, Site::LbuCount(V0)),
    (0x801F77C4, 0x3C02801D, Site::LwBase(A2)),
    (0x801F77C8, 0x24469370, Site::Nop),
    (0x801F78B0, 0x90420000, Site::LbuCount(V0)),
    (0x801F7970, 0x90420000, Site::LbuCount(V0)),
    (0x801F7990, 0x3C02801D, Site::LwBase(A2)),
    (0x801F7994, 0x24469370, Site::Nop),
    (0x801F7A7C, 0x90420000, Site::LbuCount(V0)),
    (0x801F7C18, 0x3C02801D, Site::LwBase(V1)),
    (0x801F7C1C, 0x24439370, Site::Nop),
    (0x801F7C4C, 0x90420000, Site::LbuCount(V0)),
    (0x801F7ED0, 0x90420000, Site::LbuCount(V0)),
    (0x801F7EE8, 0x3C02801D, Site::LwBase(V1)),
    (0x801F7EEC, 0x24439370, Site::Nop),
    (0x801F7F10, 0x90420000, Site::LbuCount(V0)),
    (0x801F85BC, 0x90620000, Site::LbuEnd(V0)),
    (0x801F85C8, 0x00009821, Site::LbuStart(S3)),
    (0x801F85CC, 0x3C02801D, Site::LwBase(S2)),
    (0x801F85D0, 0x24529370, Site::Nop),
    (0x801F86B8, 0x90420000, Site::LbuEnd(V0)),
    (0x801F86F0, 0x90420000, Site::LbuCount(V0)),
    (0x801F8708, 0x3C02801D, Site::LwBase(A1)),
    (0x801F870C, 0x24459370, Site::Nop),
    (0x801F8744, 0x90420000, Site::LbuCount(V0)),
    (0x801F87F4, 0x90420000, Site::LbuCount(V0)),
    (0x801F8808, 0x3C02801D, Site::Rec0Lui(V0)),
    (0x801F880C, 0x24489360, Site::Rec0Addiu(T0)),
    (0x801F8810, 0x3C02801D, Site::LwBase(A1)),
    (0x801F8814, 0x24459370, Site::Nop),
    (0x801F88A8, 0x90620000, Site::LbuCount(V0)),
    (0x801F89B4, 0x90420000, Site::LbuCount(V0)),
    (0x801F89D0, 0x3C02801D, Site::LwBase(A1)),
    (0x801F89D4, 0x24459370, Site::Nop),
    (0x801F8EB4, 0x90420000, Site::LbuEnd(V0)),
    (0x801F8EC0, 0x00009821, Site::LbuStart(S3)),
    (0x801F8EC4, 0x3C02801D, Site::LwBase(S2)),
    (0x801F8EC8, 0x24529370, Site::Nop),
    (0x801F8FB4, 0x90420000, Site::LbuEnd(V0)),
];

/// The word rewrites of a generated site table (see [`Site`]).
fn site_edits(sites: &[(u32, u32, Site)]) -> Vec<ModuleEdit> {
    let l = layout();
    sites
        .iter()
        .map(|&(va, expect, site)| {
            let replace = match site {
                Site::LbuCount(r) => m::lbu(r, GP, gp_off(3)),
                Site::LbuEnd(r) => m::lbu(r, GP, gp_off(1)),
                Site::LbuStart(r) => m::lbu(r, GP, gp_off(0)),
                Site::LwBase(r) => m::lw(r, GP, gp_off(8)),
                Site::Nop => m::nop(),
                Site::Rec0Lui(r) => m::lui(r, m::hi(l.esm_dummy)),
                Site::Rec0Addiu(r) => m::addiu(r, V0, m::lo(l.esm_dummy)),
            };
            ModuleEdit {
                va,
                expect,
                replace,
            }
        })
        .collect()
}

/// PROT 966 (Evil Seru Magic, `0xAD`) edits: 29 arms behind one dispatch. The
/// party row `actor_table[0 .. ctx[+0]]` is the victim row in retail (hidden,
/// shrunk and re-seated, struck twice, shown again); every loop over it counts
/// from zero with a pointer that steps a cell at a time, so each is pointed at
/// the opposite side with two loads: the count from the sweep scratch (`+3`,
/// the victim count) and the pointer from `+8` (`&actor_table[first victim]`).
/// The two loops whose counter doubles as the victim seat (the second hit and
/// the stager's first hit) count seats instead (`+0` .. `+1`). The caster's one
/// stage pose is raw row 6, which already is the pooled row.
pub fn edits_966() -> Vec<ModuleEdit> {
    let l = layout();
    let mut v = site_edits(ESM_SITES);
    v.push(ModuleEdit {
        va: ESM_CUE_SITE_VA,
        expect: m::jal(CUE_FN_VA),
        replace: m::j(l.guard_966),
    });
    v
}

/// PROT 951 (Chaos Flare, `0x36`) edits. A single-victim cast (the victim seat
/// is `caster[+0x1DD]`): the caster walks up, plays its cast clip (raw row 8,
/// the Recover row on a party, which becomes row `0x0A`), strikes once and
/// stays. One loop hides every seat of the victim side except the victim
/// (`beq s0,s6`, so its counter is the absolute seat), and the voice cue gets
/// the stream guard.
pub fn edits_951() -> Vec<ModuleEdit> {
    let l = layout();
    let ed = |va, expect, replace| ModuleEdit {
        va,
        expect,
        replace,
    };
    let mut v = vec![
        ed(0x801F_6D60, 0x2402_0008, stage_imm(STAGE_ROW_A)),
        ed(
            0x801F_6E38,
            0x9042_0000,
            m::lbu(V0, GP, gp_off(1)), // end seat
        ),
        ed(
            0x801F_6E44,
            m::addu(S0, ZERO, ZERO),
            m::lbu(S0, GP, gp_off(0)),
        ),
        ed(0x801F_6E50, 0x3C02_801D, m::lw(V1, GP, gp_off(8))),
        ed(0x801F_6E54, m::addiu(V1, V0, 0x9370), m::nop()),
        ed(0x801F_6E80, 0x9042_0000, m::lbu(V0, GP, gp_off(1))),
        ed(CF_CUE_SITE_VA, m::jal(CUE_FN_VA), m::j(l.guard_951)),
    ];
    // the caster's window retiming: four `lw v0,-0x6CB8(a0)` + `lw v1,0x6C(v0)`
    let (lw_v0, lw_v1) = (m::lw(V0, A0, 0x9348), m::lw(V1, V0, 0x6C));
    for site in [0x801F_6E98u32, 0x801F_6EAC, 0x801F_7214, 0x801F_7228] {
        v.push(ed(site, lw_v0, m::jal(l.cf_entry)));
        v.push(ed(site + 8, lw_v1, m::nop()));
    }
    v
}

/// The formation id the module tests (`0x8007BD0C`, a byte) for the evolved
/// Cort's fight (`0xB5`); in any other formation the same body is Dead End
/// Crisis, which sets every victim's HP to zero. Each of its ten reads becomes
/// the constant `0xB5`, so the damage roll, the camera depths and the voice
/// are the evolved Cort's wherever the cast happens.
const FC_FORMATION_READS: [(u32, u32, u32); 10] = [
    (0x801F_6AC0, 0x9043_BD0C, V1),
    (0x801F_6C08, 0x9043_BD0C, V1),
    (0x801F_6D3C, 0x9043_BD0C, V1),
    (0x801F_6E78, 0x92A2_BD0C, V0),
    (0x801F_6F30, 0x92A2_BD0C, V0),
    (0x801F_6FEC, 0x92A3_BD0C, V1),
    (0x801F_7140, 0x9043_BD0C, V1),
    (0x801F_7390, 0x9043_BD0C, V1),
    (0x801F_7590, 0x9047_BD0C, A3),
    (0x801F_784C, 0x9043_BD0C, V1),
];

const FC_SITES: &[(u32, u32, Site)] = &[
    // the damage loop (phase 3): count, first seat, first cell, end seat
    (0x801F7348, 0x90820000, Site::LbuCount(V0)),
    (0x801F7354, 0x00008821, Site::LbuStart(S1)),
    (0x801F7358, 0x03C09821, Site::LwBase(S2)),
    (0x801F7424, 0x90420000, Site::LbuEnd(V0)),
];

/// PROT 961 (Final Crisis, `0xB4`) edits. Retail's finale is cast by the first
/// monster (seat 3) on the party (seats 0..2): its phase 0 pulls the party
/// under the floor and turns seat 3 to the camera, phases 4 and 5 slow seats
/// 0..2, phase `0xFF` puts the four seats back, and the damage loop strikes
/// `actor_table[0 .. ctx[+0]]`. Everything but the damage hangs off the
/// *caster*, so a party caster needs only: the damage loop pointed at the
/// enemy row (count from the sweep scratch `+3`, absolute seats `+0 .. +1`,
/// first cell `+8`; the `move s3,fp` it displaced goes one word down); the
/// seat shuffle skipped (phase 0's four-seat loop and phase `0xFF`'s restore:
/// the caster stays where it stands); the slow-down of seats 1 and 2 aimed at
/// the caster (a party of one has no such seats, and the write would land on a
/// null pointer); the ten formation reads forced to `0xB5`; and the voice
/// cue guarded like the other modules'. The caster's pose is raw row 6, which
/// already is the pooled row.
pub fn edits_961() -> Vec<ModuleEdit> {
    let l = layout();
    let ed = |va, expect, replace| ModuleEdit {
        va,
        expect,
        replace,
    };
    let mut v = site_edits(FC_SITES);
    v.push(ed(0x801F_735C, 0x0280_9021, 0x03C0_9821)); // move s2,s4 -> move s3,fp
    for (va, expect, reg) in FC_FORMATION_READS {
        v.push(ed(va, expect, m::addiu(reg, ZERO, 0xB5)));
    }
    v.push(ed(0x801F_6AE4, 0x0100_2021, m::j(0x801F_6BD8)));
    v.push(ed(0x801F_77D4, 0x3C02_8020, m::j(0x801F_7830)));
    let to_caster = m::addu(V1, S2, ZERO);
    for (va, expect) in [
        (0x801F_744C, 0x8E83_0004),
        (0x801F_7458, 0x8E83_0008),
        (0x801F_752C, 0x8E83_0004),
        (0x801F_7538, 0x8E83_0008),
    ] {
        v.push(ed(va, expect, to_caster));
    }
    v.push(ed(FC_CUE_SITE_VA, m::jal(CUE_FN_VA), m::j(l.guard_961)));
    v
}

/// PROT 963 (Genocidal Cannon, `0xB3`) edits. Retail's cannon strikes the
/// whole party: the victim row is hidden (arm 1), struck (arm 12) and shown
/// again (arms 11 / 12 / 14 / 17 and the show at arm 8) by loops over
/// `actor_table[0 .. ctx[+0]]`, each pointed at the opposite side like
/// Evil Seru Magic's; the damage loop counts seats. The caster stages raw row 8
/// (row `0x0A`) and, like Chaos Flare, retimes its own clip's hold window
/// through the first monster's record (four reads, redirected to the caster's
/// row-A entry); the stager that seats the flares reads `actor_table[3]` (two
/// reads, redirected to the caster); and the last arm writes the first
/// monster's readef group byte (`0x1C`), which would re-point the *enemy's*
/// next stream, so it is dropped.
pub fn edits_963() -> Vec<ModuleEdit> {
    let l = layout();
    let mut v = site_edits(GC_SITES);
    let ed = |va, expect, replace| ModuleEdit {
        va,
        expect,
        replace,
    };
    v.push(ed(
        0x801F_6EB8,
        m::addiu(V1, ZERO, 8),
        m::addiu(V1, ZERO, u16::from(STAGE_ROW_A)),
    ));
    v.push(ed(GC_CUE_SITE_VA, m::jal(CUE_FN_VA), m::j(l.guard_963)));
    let (lw_v1, lw_v0_a1, lw_v0_a0) = (
        m::lw(V1, V0, 0x6C),
        m::lw(V0, A1, 0x9348),
        m::lw(V0, A0, 0x9348),
    );
    for (site, expect) in [
        (0x801F_6ED4u32, lw_v0_a1),
        (0x801F_6EE8, lw_v0_a1),
        (0x801F_71D0, lw_v0_a0),
        (0x801F_71E4, lw_v0_a0),
    ] {
        v.push(ed(site, expect, m::jal(l.cf_entry)));
        v.push(ed(site + 8, lw_v1, m::nop()));
    }
    v.push(ed(0x801F_80F8, m::sb(V0, V1, 0x1C), m::nop()));
    for site in [0x801F_8218u32, 0x801F_8300] {
        v.push(ed(site, m::addiu(V1, V1, 0x9370), m::jal(l.caster_cell)));
    }
    v
}

const GC_SITES: &[(u32, u32, Site)] = &[
    (0x801F6BE4, 0x90420000, Site::LbuCount(V0)),
    (0x801F6BF8, 0x3C02801D, Site::LwBase(V1)),
    (0x801F6BFC, 0x24439370, Site::Nop),
    (0x801F6C20, 0x90420000, Site::LbuCount(V0)),
    (0x801F78B4, 0x90420000, Site::LbuCount(V0)),
    (0x801F78D0, 0x3C02801D, Site::LwBase(V1)),
    (0x801F78D4, 0x24439370, Site::Nop),
    (0x801F78F8, 0x90420000, Site::LbuCount(V0)),
    (0x801F7C00, 0x90420000, Site::LbuEnd(V0)),
    (0x801F7C0C, 0x00009021, Site::LbuStart(S2)),
    (0x801F7C10, 0x3C02801D, Site::LwBase(S3)),
    (0x801F7C14, 0x24539370, Site::Nop),
    (0x801F7D3C, 0x90420000, Site::LbuEnd(V0)),
    (0x801F7D8C, 0x90420000, Site::LbuCount(V0)),
    (0x801F7DA0, 0x3C02801D, Site::LwBase(V1)),
    (0x801F7DA4, 0x24439370, Site::Nop),
    (0x801F7DBC, 0x90420000, Site::LbuCount(V0)),
    (0x801F7EDC, 0x90420000, Site::LbuCount(V0)),
    (0x801F7EEC, 0x3C02801D, Site::LwBase(V1)),
    (0x801F7EF0, 0x24439370, Site::Nop),
    (0x801F7F08, 0x90420000, Site::LbuCount(V0)),
    (0x801F8020, 0x90620000, Site::LbuCount(V0)),
    (0x801F802C, 0x3C02801D, Site::LwBase(V1)),
    (0x801F8034, 0x24439370, Site::Nop),
    (0x801F8058, 0x90420000, Site::LbuCount(V0)),
    (0x801F8114, 0x90620000, Site::LbuCount(V0)),
    (0x801F8130, 0x3C02801D, Site::LwBase(V1)),
    (0x801F8134, 0x24439370, Site::Nop),
    (0x801F8164, 0x90420000, Site::LbuCount(V0)),
];

/// Module PROT entry and edits for a spell.
pub fn module_for(spell: u8) -> Option<(usize, Vec<ModuleEdit>)> {
    match spell {
        SPELL_HYPER_CRUSH => Some((936, edits_936())),
        SPELL_HYPER_LIGHTNING => Some((937, edits_937())),
        SPELL_GUILTY_CROSS => Some((944, edits_944())),
        SPELL_MYSTIC_CIRCLE => Some((938, edits_938())),
        SPELL_MYSTIC_SHIELD => Some((940, edits_940())),
        SPELL_EVIL_SERU => Some((966, edits_966())),
        SPELL_CHAOS_FLARE => Some((951, edits_951())),
        SPELL_GENOCIDAL_CANNON => Some((963, edits_963())),
        SPELL_FINAL_CRISIS => Some((961, edits_961())),
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

/// Replace one word of the battle overlay (PROT 898), verifying the retail one.
fn patch_overlay_word(
    patcher: &mut DiscPatcher,
    va: u32,
    expect: u32,
    replace: u32,
) -> Result<bool> {
    let entry = patcher.read_entry(898)?;
    let at = (va - 0x801C_E818) as usize;
    let word = u32::from_le_bytes(entry[at..at + 4].try_into()?);
    if word == replace {
        return Ok(false);
    }
    if word != expect {
        bail!("overlay {va:#010x}: expected {expect:#010x}, found {word:#010x}");
    }
    patcher.patch_prot_entry(898, at as u64, &replace.to_le_bytes())?;
    Ok(true)
}

/// Replace one word of `SCUS_942.54`, verifying the retail one.
fn patch_scus_word(patcher: &mut DiscPatcher, va: u32, expect: u32, replace: u32) -> Result<bool> {
    let scus = patcher
        .read_named_file(SCUS_NAME)
        .context("read SCUS_942.54")?;
    let off = legaia_game_tables::item_names::file_offset_for_va(&scus, va)
        .with_context(|| format!("resolve the file offset of {va:#010x}"))?;
    let word = u32::from_le_bytes(scus[off..off + 4].try_into()?);
    if word == replace {
        return Ok(false);
    }
    if word != expect {
        bail!("SCUS {va:#010x}: expected {expect:#010x}, found {word:#010x}");
    }
    patcher.patch_named_file(SCUS_NAME, off as u64, &replace.to_le_bytes())?;
    Ok(true)
}

/// The party-mesh assembler (`FUN_80052FA0`) allocates its work buffer with
/// `lui a1,1; ori a1,a1,0x9000`. Raise it to hold `need` bytes (rounded up with
/// a little headroom); returns the new size.
pub fn grow_member_alloc(patcher: &mut DiscPatcher, need: usize) -> Result<u32> {
    const SITE: u32 = 0x8005_301C;
    let size = (need as u32 + 0x400).next_multiple_of(0x100);
    if !(0x1_9000..0x2_0000).contains(&size) {
        bail!("member buffer of {size:#x} bytes is outside what one `ori` can ask for");
    }
    patch_scus_word(
        patcher,
        SITE,
        m::ori(5, 5, 0x9000),
        m::ori(5, 5, (size & 0xFFFF) as u16),
    )?;
    Ok(size)
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
    if l.owner + 1 > ARENA2_END_VA
        || l.readef + 1 > ARENA2_END_VA
        || l.rows + (ROW_SLOTS * 4) as u32 > l.readef
    {
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
    if readef_base != 0 && specials.iter().any(|s| s.spell == SPELL_MYSTIC_CIRCLE) {
        changed |= write_scus(patcher, l.guard_938, &assemble_mc_guard())?;
    }
    if readef_base != 0 && specials.iter().any(|s| s.spell == SPELL_CHAOS_FLARE) {
        changed |= write_scus(patcher, l.guard_951, &assemble_cf_guard())?;
    }
    if readef_base != 0 && specials.iter().any(|s| s.spell == SPELL_FINAL_CRISIS) {
        changed |= write_scus(patcher, l.guard_961, &assemble_fc_guard())?;
    }
    if readef_base != 0 && specials.iter().any(|s| s.spell == SPELL_GENOCIDAL_CANNON) {
        changed |= write_scus(patcher, l.guard_963, &assemble_gc_guard())?;
        changed |= write_scus(patcher, l.caster_cell, &assemble_caster_cell())?;
    }
    if readef_base != 0
        && specials
            .iter()
            .any(|s| matches!(s.spell, SPELL_CHAOS_FLARE | SPELL_GENOCIDAL_CANNON))
    {
        changed |= write_scus(patcher, l.cf_entry, &assemble_cf_entry())?;
    }
    if readef_base != 0 && specials.iter().any(|s| s.spell == SPELL_EVIL_SERU) {
        changed |= write_scus(patcher, l.guard_966, &assemble_esm_guard())?;
        changed |= write_scus(patcher, l.esm_dummy, &assemble_esm_dummy(&l))?;
    }
    if specials.iter().any(|s| s.spell == SPELL_MYSTIC_SHIELD) {
        for (va, code) in [
            (l.shield_finisher, assemble_shield_finisher(&l)),
            (l.shield_store, assemble_shield_store(&l)),
            (l.shield_caster, assemble_shield_caster()),
            (l.shield_pass, assemble_shield_pass(&l)),
        ] {
            if va + code.len() as u32 > HOOK_END_VA {
                bail!("a shield routine does not fit its gap");
            }
            changed |= write_scus(patcher, va, &code)?;
        }
        changed |= patch_overlay_word(
            patcher,
            SHIELD_FINISHER_SITE_VA,
            m::bne(V1, ZERO, 14),
            m::j(l.shield_finisher),
        )?;
        changed |= patch_scus_word(
            patcher,
            SHIELD_PASS_SITE_VA,
            m::lw(V0, A1, 0xC),
            m::jal(l.shield_pass),
        )?;
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
        let routed: Vec<Special> = [0x37u8, 0x4B, 0xB7, 0xAD]
            .iter()
            .map(|&spell| Special {
                constant: 0x27,
                spell,
            })
            .collect();
        cpu.load(l.rows, &row_table(&routed, &[]).unwrap());
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
        assert_eq!(readef_request(0x99, 0x36, 0), (0, 0), "not routed");
        assert_eq!(readef_request(0xB7, 0, 0), (0, 0), "no group configured");
    }

    /// Run the shield finisher hook. Returns `(damage, where it left)`.
    fn finisher(defender_party: bool, owner_seat: u8, shield: bool) -> (u32, u32) {
        let mut cpu = Cpu::new();
        let l = layout();
        cpu.load(l.shield_finisher, &assemble_shield_finisher(&l));
        cpu.wr32(0x8007_BD84, u32::from(shield) * 0x1234);
        cpu.wr8(l.owner, owner_seat);
        cpu.wr32(0x8010_0000, 100); // damage
        cpu.wr32(0x8010_0010, 20); // floor
        cpu.r[S0 as usize] = 0x8010_0000;
        cpu.r[S2 as usize] = 0x8010_0010;
        cpu.r[V1 as usize] = u32::from(defender_party);
        cpu.r[GP as usize] = GP_VA;
        cpu.pc = l.shield_finisher;
        let at = cpu.run_until(&[SHIELD_FINISHER_PARTY_VA, SHIELD_FINISHER_DONE_VA]);
        (cpu.rd32(0x8010_0000), at)
    }

    #[test]
    fn a_hosts_shield_halves_hits_on_the_party() {
        // party owner: a party defender is halved (excess over the floor)
        assert_eq!(finisher(true, 0, true), (60, SHIELD_FINISHER_PARTY_VA));
        // ... and an enemy defender is not
        assert_eq!(finisher(false, 0, true), (100, SHIELD_FINISHER_DONE_VA));
    }

    #[test]
    fn a_monsters_shield_keeps_its_retail_side() {
        assert_eq!(finisher(false, 3, true), (60, SHIELD_FINISHER_DONE_VA));
        assert_eq!(finisher(true, 3, true), (100, SHIELD_FINISHER_PARTY_VA));
    }

    #[test]
    fn no_shield_no_halving() {
        assert_eq!(finisher(true, 0, false), (100, SHIELD_FINISHER_PARTY_VA));
        assert_eq!(finisher(false, 3, false), (100, SHIELD_FINISHER_DONE_VA));
    }

    #[test]
    fn the_shield_store_records_handle_and_owner() {
        let mut cpu = Cpu::new();
        let l = layout();
        cpu.load(l.shield_store, &assemble_shield_store(&l));
        cpu.wr32(CTX_PTR_VA, CTX);
        cpu.wr8(CTX + 0x13, 2);
        cpu.r[V0 as usize] = 0xABCD;
        cpu.r[V1 as usize] = 0x8008_0000;
        cpu.r[GP as usize] = GP_VA;
        cpu.r[RA as usize] = RETURN_VA;
        cpu.pc = l.shield_store;
        cpu.run_until(&[RETURN_VA]);
        assert_eq!(cpu.rd32(0x8007_BD84), 0xABCD);
        assert_eq!(cpu.rd8(l.owner), 2);
    }

    #[test]
    fn the_shield_follows_the_caster_not_seat_three() {
        let mut cpu = Cpu::new();
        cpu.load(layout().shield_caster, &assemble_shield_caster());
        cpu.wr32(CTX_PTR_VA, CTX);
        cpu.wr8(CTX + 0x13, 1);
        cpu.r[RA as usize] = RETURN_VA;
        cpu.pc = layout().shield_caster;
        cpu.run_until(&[RETURN_VA]);
        // 0xC(s0) is the caster's table cell
        assert_eq!(cpu.r[S0 as usize] + 0xC, ACTOR_TABLE_VA + 4);
        // a monster caster at seat 3 keeps retail's `0xC(table)`
        cpu.wr8(CTX + 0x13, 3);
        cpu.r[RA as usize] = RETURN_VA;
        cpu.pc = layout().shield_caster;
        cpu.run_until(&[RETURN_VA]);
        assert_eq!(cpu.r[S0 as usize] + 0xC, ACTOR_TABLE_VA + 12);
    }

    #[test]
    fn the_per_frame_pass_reads_the_owner() {
        let mut cpu = Cpu::new();
        let l = layout();
        cpu.load(l.shield_pass, &assemble_shield_pass(&l));
        cpu.wr8(l.owner, 0);
        cpu.wr32(ACTOR_TABLE_VA, 0x8012_3456);
        cpu.wr32(ACTOR_TABLE_VA + 12, 0x8012_9999);
        cpu.r[GP as usize] = GP_VA;
        cpu.r[RA as usize] = RETURN_VA;
        cpu.pc = l.shield_pass;
        cpu.run_until(&[RETURN_VA]);
        assert_eq!(cpu.r[V0 as usize], 0x8012_3456);
        assert_eq!(cpu.r[A1 as usize] + 0xC, ACTOR_TABLE_VA);
        cpu.wr8(l.owner, 3);
        cpu.r[RA as usize] = RETURN_VA;
        cpu.pc = l.shield_pass;
        cpu.run_until(&[RETURN_VA]);
        assert_eq!(cpu.r[V0 as usize], 0x8012_9999);
    }

    #[test]
    fn mystic_circle_waits_for_the_stream_before_its_voice() {
        for (stage, resumed) in [(0u8, true), (1, false), (4, false)] {
            let mut cpu = Cpu::new();
            let l = layout();
            cpu.load(l.guard_938, &assemble_mc_guard());
            cpu.load_words(CUE_FN_VA, &[m::jr(RA), m::nop()]);
            cpu.wr32(CTX_PTR_VA, CTX);
            cpu.wr8(CTX + 0x276, stage);
            cpu.r[RA as usize] = RETURN_VA;
            cpu.pc = l.guard_938;
            let at = cpu.run_until(&[MC_CUE_RESUME_VA, MC_EPILOGUE_VA]);
            assert_eq!(at == MC_CUE_RESUME_VA, resumed, "stage {stage}");
            assert_eq!(cpu.r[A0 as usize] == u32::from(MC_CUE_ID), resumed);
        }
    }

    #[test]
    fn chaos_flare_reads_the_casters_row_a_entry() {
        let mut cpu = Cpu::new();
        let l = layout();
        cpu.load(l.cf_entry, &assemble_cf_entry());
        cpu.wr32(CTX_PTR_VA, CTX);
        cpu.wr8(CTX + 0x13, 1);
        cpu.wr32(RECORD0_TABLE_VA + 4, REC0);
        cpu.wr32(REC0 + 0x28, REC0 + 0x1234); // row 0x0A
        cpu.r[RA as usize] = RETURN_VA;
        cpu.pc = l.cf_entry;
        cpu.run_until(&[RETURN_VA]);
        assert_eq!(cpu.r[V1 as usize], REC0 + 0x1234);
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
        for code in [
            assemble_shield_finisher(&l),
            assemble_shield_store(&l),
            assemble_shield_caster(),
            assemble_shield_pass(&l),
            assemble_mc_guard(),
            assemble_esm_guard(),
            assemble_cf_guard(),
            assemble_cf_entry(),
            assemble_gc_guard(),
            assemble_caster_cell(),
        ] {
            assert!(crate::hazard::load_hazards(&words(&code)).is_empty());
        }
    }

    #[test]
    fn everything_fits() {
        let l = layout();
        assert!(l.prep as usize + assemble_prep(&l).len() <= ARENA1_END_VA as usize);
        assert!(l.rows as usize + ROW_SLOTS * 4 <= l.readef as usize);
        assert!(l.readef < ARENA2_END_VA && l.owner < ARENA2_END_VA);
        for (va, code) in [
            (l.shield_finisher, assemble_shield_finisher(&l)),
            (l.shield_store, assemble_shield_store(&l)),
            (l.shield_caster, assemble_shield_caster()),
            (l.shield_pass, assemble_shield_pass(&l)),
        ] {
            assert!((va + code.len() as u32) <= HOOK_END_VA);
        }
        // Songi's third form shares the run with the shield routines (never
        // installed together): its four routines fit in it, in order
        assert!(l.guard_951 + assemble_cf_guard().len() as u32 <= l.guard_963);
        assert!(l.guard_963 + assemble_gc_guard().len() as u32 <= l.cf_entry);
        assert!(l.cf_entry + assemble_cf_entry().len() as u32 <= l.caster_cell);
        assert!(l.caster_cell + assemble_caster_cell().len() as u32 <= l.shield_pass);
        // the guards and the stand-in table sit behind them
        assert!(l.shield_pass + assemble_shield_pass(&l).len() as u32 <= l.guard_938);
        assert!(l.guard_938 + assemble_mc_guard().len() as u32 <= l.guard_966);
        assert!(l.guard_966 + assemble_esm_guard().len() as u32 <= l.esm_dummy);
        assert!(l.esm_dummy + assemble_esm_dummy(&l).len() as u32 <= HOOK_END_VA);
        // the routines do not overlap
        assert!(l.hook + assemble_readef_request(&l).len() as u32 <= l.shield_finisher);
        assert!(l.shield_finisher + assemble_shield_finisher(&l).len() as u32 <= l.shield_store);
        assert!(l.shield_store + assemble_shield_store(&l).len() as u32 <= l.shield_caster);
        assert!(l.shield_caster + assemble_shield_caster().len() as u32 <= l.shield_pass);
        assert!(l.hook as usize + assemble_readef_request(&l).len() <= HOOK_END_VA as usize);
        assert!(
            SCUS_GAP_VA as usize + assemble_route(l.table, Some(l.hook)).len()
                <= SCUS_GAP_END_VA as usize
        );
        // gp-relative scratch reaches: 0x8007AFF8 - 0x8007B318 = -0x320
        assert_eq!(gp_off(0), 0xFCE0);
    }
}
