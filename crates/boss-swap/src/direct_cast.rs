//! Stage 2: the host's special techniques are the boss's own specials, cast
//! where the host stands.
//!
//! A *cast art* is an art whose bank record plays a boss cast clip. Its input
//! stays the retail one (Somersault is Up, Down, Up) and the retail tokenizer
//! still turns it into `[arrows.., 0x19, constant]`. Two hooks in the
//! battle-action overlay (PROT 898, base `0x801CE818`) change what happens
//! next:
//!
//! 1. **Attack begin** (`FUN_801E295C` state `0x14`). Retail calls
//!    `FUN_8004E2F0(attacker, target)` at `0x801E31F4` and walks in unless the
//!    range is zero. The call goes through a stub that runs the real range
//!    function and then, for seat 0 only, scans the queue at `actor[+0x1DF]`
//!    for a `0x19` starter followed by a cast-art constant. On a hit the
//!    whole queue becomes `[0x19, constant]` - the arrows the art consumed
//!    and any other swings are dropped - and the stub returns range `0`, so
//!    the state stores `0x1E` and the cast plays in place. Queues without a
//!    cast art are left exactly as retail built them, so melee chains and the
//!    other arts behave as before.
//! 2. **Move id fetch.** Two sites read the queue head as the move id, map it
//!    through the move-power id map and index the record table: the effect
//!    terminator that installs the record (`0x801DF250`, the projectile) and
//!    the per-hit damage setup (`0x801E187C`). Both load `actor[+0x1DF]`, a
//!    fixed head byte, which for a cast art is `0x19` and says nothing about
//!    the spell. Each `lbu v1, 0x1DF(v1)` becomes a `jal` to a second stub that,
//!    for seat 0 on an attack-category action, reads the byte being played
//!    (`actor[+0x1DE + ctx[+0x15]]`, the strike loop's cursor is one past it)
//!    and, when that is a cast-art constant, returns the **spell id** the art
//!    casts. The spell id is already a key of the id map, so no map entry is
//!    patched and no monster's move changes. Anything else falls through to
//!    the retail load.
//!
//! 3. **Commit-time range snap** (SCUS, `0x8004BFA8`). Forcing range 0 at
//!    state `0x14` is not enough: every clip the strike loop commits runs a
//!    block at the end of the anim commit that calls the range function and
//!    walks the attacker to the range boundary in a single frame - the host
//!    would be snapped next to the target the moment the starter commits. The
//!    `jal` at the block's first range call goes to a third stub that, when
//!    the attacker's queue is exactly `[starter, cast constant, 0]` (the shape
//!    the first hook leaves), jumps to the block's exit (the function's own
//!    epilogue, so the stack frame is intact) and otherwise tail-calls the
//!    real range function.
//!
//! Register contract (read off the disassembly): at `0x801E31F4` `s3` is the
//! acting actor, `s5` is `ctx + 0x11` and no temporary is live across the
//! call; at both fetch sites `v1` holds the actor, `v0` the map base (kept) and
//! `t0..t3` are dead.

use anyhow::{Context, Result, bail};
use legaia_code_hooks::arenas::{ARENA1_END_VA, ARENA1_VA, SCUS_GAP_END_VA, SCUS_GAP_VA};
use legaia_code_hooks::mips::{
    self as m, A0, RA, S1, S3, SP, T0, T1, T2, T3, T4, T5, V0, V1, ZERO,
};
use legaia_disc_patch::disc::DiscPatcher;

/// Battle-action overlay and its link base.
const BATTLE_OVERLAY_PROT: usize = 898;
const BATTLE_OVERLAY_BASE: u32 = 0x801C_E818;
/// The range-check call in state `0x14`.
const RANGE_CALL_VA: u32 = 0x801E_31F4;
/// `FUN_8004E2F0(attacker, target)`: the attack range metric.
const RANGE_FN_VA: u32 = 0x8004_E2F0;
/// The two `lbu v1, 0x1DF(v1)` move-id fetches.
const FETCH_SITES: [u32; 2] = [0x801D_F250, 0x801E_187C];
/// Retail word at both fetch sites.
const FETCH_RETAIL: u32 = 0x9063_01DF; // lbu v1, 0x1DF(v1)
/// The range-normalising block of the anim commit in `SCUS_942.54`: while the
/// action state is `0x1E` and the committing actor is the attacker, every clip
/// commit calls `FUN_8004E2F0` at this site and then walks the attacker to
/// the range boundary in one frame (forward when out of range, backward when
/// inside it). Its exit is the function epilogue.
const SNAP_SITE_VA: u32 = 0x8004_BFA8;
const SNAP_EXIT_VA: u32 = 0x8004_C11C;
/// Actor pointer table and the battle context pointer.
const ACTOR_TABLE_VA: u32 = 0x801C_9370;
const CTX_PTR_VA: u32 = 0x8007_BD24;
const SCUS_NAME: &str = "SCUS_942.54";

/// First art constant and the number of constants the table covers
/// (`0x10..=0x30`: every row of the 33-record art bank).
pub const TABLE_BASE: u8 = 0x10;
pub const TABLE_LEN: usize = 0x21;

/// One cast art: the art's queue constant and the spell id it casts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CastArt {
    pub constant: u8,
    pub spell: u8,
}

/// A tiny assembler: words plus branch labels resolved at the end.
struct Asm {
    w: Vec<u32>,
    fix: Vec<(usize, usize, fn(i16) -> u32)>, // (word index, label, encoder)
    labels: Vec<Option<usize>>,
}

impl Asm {
    fn new() -> Self {
        Self {
            w: Vec::new(),
            fix: Vec::new(),
            labels: Vec::new(),
        }
    }
    fn label(&mut self) -> usize {
        self.labels.push(None);
        self.labels.len() - 1
    }
    fn bind(&mut self, l: usize) {
        self.labels[l] = Some(self.w.len());
    }
    fn op(&mut self, word: u32) {
        self.w.push(word);
    }
    /// Branch with a pre-built encoder `f(offset)`; the delay slot is the
    /// caller's next `op`.
    fn br(&mut self, l: usize, f: fn(i16) -> u32) {
        self.fix.push((self.w.len(), l, f));
        self.w.push(0);
    }
    fn finish(mut self) -> Vec<u32> {
        for &(at, l, f) in &self.fix {
            let to = self.labels[l].expect("unbound label");
            self.w[at] = f((to as i32 - at as i32 - 1) as i16);
        }
        self.w
    }
}

fn beq_zz(o: i16) -> u32 {
    m::beq(ZERO, ZERO, o)
}
fn bne_a0(o: i16) -> u32 {
    m::bne(A0, ZERO, o)
}
fn beq_t1z(o: i16) -> u32 {
    m::beq(T1, ZERO, o)
}
fn beq_t1t2(o: i16) -> u32 {
    m::beq(T1, T2, o)
}
fn beq_t0t1(o: i16) -> u32 {
    m::beq(T0, T1, o)
}
fn bne_t0t1(o: i16) -> u32 {
    m::bne(T0, T1, o)
}
fn bne_t2z(o: i16) -> u32 {
    m::bne(T2, ZERO, o)
}
fn bne_t1t2(o: i16) -> u32 {
    m::bne(T1, T2, o)
}
fn beq_t2z(o: i16) -> u32 {
    m::beq(T2, ZERO, o)
}
fn beq_t0z(o: i16) -> u32 {
    m::beq(T0, ZERO, o)
}
fn bne_v1t0(o: i16) -> u32 {
    m::bne(V1, T0, o)
}
fn bne_t3t2(o: i16) -> u32 {
    m::bne(T3, T2, o)
}
fn bne_t4z(o: i16) -> u32 {
    m::bne(T4, ZERO, o)
}

/// Addresses of the injected pieces.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    /// Attack-begin stub (SCUS gap).
    pub begin_va: u32,
    /// Cast-id table, `TABLE_LEN` bytes (SCUS gap, after the stub).
    pub table_va: u32,
    /// Move-id fetch stub (ARENA1).
    pub fetch_va: u32,
}

/// Assemble the SCUS-gap image: the attack-begin stub then the table.
pub fn assemble_begin(casts: &[CastArt]) -> (Vec<u8>, u32) {
    // The table address depends on the code length, which does not depend on
    // the address: assemble once with a placeholder to learn the length.
    let len = begin_words(0).len() as u32 * 4;
    let table_va = SCUS_GAP_VA + len;
    let words = begin_words(table_va);
    debug_assert_eq!(words.len() as u32 * 4, len);
    let mut out: Vec<u8> = words.iter().flat_map(|x| x.to_le_bytes()).collect();
    out.extend_from_slice(&table(casts));
    (out, table_va)
}

fn table(casts: &[CastArt]) -> [u8; TABLE_LEN] {
    let mut t = [0u8; TABLE_LEN];
    for c in casts {
        let i = usize::from(c.constant.wrapping_sub(TABLE_BASE));
        if i < TABLE_LEN {
            t[i] = c.spell;
        }
    }
    t
}

fn begin_words(table_va: u32) -> Vec<u32> {
    let mut a = Asm::new();
    let (ret, scan, next, found, starter) = (a.label(), a.label(), a.label(), a.label(), a.label());
    a.op(m::addiu(SP, SP, 0xFFE8));
    a.op(m::sw(RA, SP, 0x14));
    a.op(m::sw(A0, SP, 0x10));
    a.op(m::jal(RANGE_FN_VA));
    a.op(m::nop());
    a.op(m::lw(A0, SP, 0x10));
    a.op(m::lw(RA, SP, 0x14));
    a.br(ret, bne_a0); // only seat 0
    a.op(m::addiu(SP, SP, 0x18)); // delay: frame release
    a.op(m::addiu(T0, S3, 0x1DF)); // queue cursor
    a.op(m::addiu(T4, ZERO, 0x12)); // scan bound
    a.bind(scan);
    a.op(m::lbu(T1, T0, 0));
    a.op(m::addiu(T2, ZERO, 0x19)); // (load-delay filler)
    a.br(ret, beq_t1z); // terminator
    a.op(m::nop());
    a.br(starter, beq_t1t2); // 0x19: a known art
    a.op(m::addiu(T2, ZERO, 0x1A)); // delay
    a.br(next, bne_t1t2); // not 0x1A (newly learned art) either
    a.op(m::nop());
    a.bind(starter);
    a.op(m::addu(T5, T1, ZERO)); // remember which starter it was
    a.op(m::lbu(T1, T0, 1)); // art constant
    a.op(m::nop()); // load delay
    a.op(m::addiu(T2, T1, 0xFFF0)); // constant - 0x10
    a.op(m::sltiu(T3, T2, TABLE_LEN as u16));
    a.br(next, beq_t3z); // out of the table
    a.op(m::lui(T3, m::hi(table_va))); // delay
    a.op(m::addiu(T3, T3, m::lo(table_va)));
    a.op(m::addu(T3, T3, T2));
    a.op(m::lbu(T3, T3, 0));
    a.op(m::nop());
    a.br(next, beq_t3z); // not a cast art
    a.op(m::nop());
    a.br(found, beq_zz);
    a.op(m::nop());
    a.bind(next);
    a.op(m::addiu(T0, T0, 1));
    a.op(m::addiu(T4, T4, 0xFFFF));
    a.br(scan, bne_t4z);
    a.op(m::nop());
    a.br(ret, beq_zz);
    a.op(m::nop());
    a.bind(found);
    a.op(m::sb(T5, S3, 0x1DF));
    a.op(m::sb(T1, S3, 0x1E0));
    a.op(m::sb(ZERO, S3, 0x1E1));
    a.op(m::addu(V0, ZERO, ZERO)); // in range
    a.bind(ret);
    a.op(m::jr(RA));
    a.op(m::nop());
    a.finish()
}

fn beq_t3z(o: i16) -> u32 {
    m::beq(T3, ZERO, o)
}

/// Assemble the move-id fetch stub.
pub fn assemble_fetch(table_va: u32) -> Vec<u8> {
    let mut a = Asm::new();
    let retail = a.label();
    a.op(m::lui(T0, m::hi(ACTOR_TABLE_VA)));
    a.op(m::lw(T0, T0, m::lo(ACTOR_TABLE_VA))); // actor of seat 0
    a.op(m::lui(T1, m::hi(CTX_PTR_VA)));
    a.op(m::lw(T1, T1, m::lo(CTX_PTR_VA))); // ctx
    a.br(retail, bne_v1t0); // not the host seat
    a.op(m::lbu(T2, V1, 0x1DE)); // delay: action category
    a.op(m::addiu(T3, ZERO, 3));
    a.br(retail, bne_t3t2); // not an Attack
    a.op(m::lbu(T2, T1, 0x15)); // delay: strike cursor
    a.op(m::nop());
    a.br(retail, beq_t2z); // cursor 0: nothing played yet
    a.op(m::addu(T3, V1, T2)); // delay: actor + cursor
    a.op(m::lbu(T3, T3, 0x1DE)); // byte being played
    a.op(m::nop());
    a.op(m::addiu(T2, T3, 0xFFF0));
    a.op(m::sltiu(T0, T2, TABLE_LEN as u16));
    a.br(retail, beq_t0z); // not an art constant
    a.op(m::lui(T0, m::hi(table_va))); // delay
    a.op(m::addiu(T0, T0, m::lo(table_va)));
    a.op(m::addu(T0, T0, T2));
    a.op(m::lbu(T0, T0, 0));
    a.op(m::nop());
    a.br(retail, beq_t0z); // not a cast art
    a.op(m::nop());
    a.op(m::jr(RA));
    a.op(m::addu(V1, T0, ZERO)); // delay: the spell id
    a.bind(retail);
    a.op(m::lbu(V1, V1, 0x1DF));
    a.op(m::jr(RA));
    a.op(m::nop());
    a.finish().iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// VA of the snap stub: right after the fetch stub in ARENA1.
pub fn snap_va() -> u32 {
    ARENA1_VA + assemble_fetch(0).len() as u32
}

/// Assemble the commit-time snap stub (`s1` is the committing actor, `a0` its
/// seat).
pub fn assemble_snap(table_va: u32) -> Vec<u8> {
    let mut a = Asm::new();
    let (real, chk) = (a.label(), a.label());
    a.br(real, bne_a0); // only seat 0
    a.op(m::nop());
    a.op(m::lbu(T0, S1, 0x1DF)); // first queue byte
    a.op(m::addiu(T1, ZERO, 0x19));
    a.br(chk, beq_t0t1); // 0x19
    a.op(m::addiu(T1, ZERO, 0x1A)); // delay
    a.br(real, bne_t0t1); // not 0x1A either
    a.op(m::nop());
    a.bind(chk);
    a.op(m::lbu(T0, S1, 0x1E0)); // art constant
    a.op(m::lbu(T2, S1, 0x1E1)); // must be the terminator
    a.op(m::addiu(T1, T0, 0xFFF0));
    a.op(m::sltiu(T3, T1, TABLE_LEN as u16));
    a.br(real, beq_t3z);
    a.op(m::nop());
    a.br(real, bne_t2z);
    a.op(m::lui(T3, m::hi(table_va))); // delay
    a.op(m::addiu(T3, T3, m::lo(table_va)));
    a.op(m::addu(T3, T3, T1));
    a.op(m::lbu(T3, T3, 0));
    a.op(m::nop());
    a.br(real, beq_t3z); // not a cast art
    a.op(m::nop());
    a.op(m::j(SNAP_EXIT_VA));
    a.op(m::nop());
    a.bind(real);
    a.op(m::j(RANGE_FN_VA));
    a.op(m::nop());
    a.finish().iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// Write the stubs and redirect the three overlay sites. Returns `false`
/// when everything is already in place.
pub fn install(patcher: &mut DiscPatcher, casts: &[CastArt]) -> Result<bool> {
    if casts.is_empty() {
        bail!("direct cast needs at least one cast art");
    }
    let (begin, table_va) = assemble_begin(casts);
    if SCUS_GAP_VA + begin.len() as u32 > SCUS_GAP_END_VA {
        bail!("direct-cast stub does not fit the SCUS gap");
    }
    let fetch = assemble_fetch(table_va);
    let snap = assemble_snap(table_va);
    if snap_va() + snap.len() as u32 > ARENA1_END_VA {
        bail!("direct-cast stubs do not fit ARENA1");
    }
    let mut changed = false;
    changed |= write_scus(patcher, SCUS_GAP_VA, &begin)?;
    changed |= write_scus(patcher, ARENA1_VA, &fetch)?;
    changed |= write_scus(patcher, snap_va(), &snap)?;
    changed |= redirect_scus_call(patcher, SNAP_SITE_VA, snap_va())?;

    let entry = patcher.read_entry(BATTLE_OVERLAY_PROT)?;
    let word_at = |va: u32| -> Result<(usize, u32)> {
        let at = (va - BATTLE_OVERLAY_BASE) as usize;
        let w = u32::from_le_bytes(entry[at..at + 4].try_into()?);
        Ok((at, w))
    };
    let sites = [
        (
            RANGE_CALL_VA,
            m::jal(RANGE_FN_VA),
            m::jal(SCUS_GAP_VA),
            "range call",
        ),
        (
            FETCH_SITES[0],
            FETCH_RETAIL,
            m::jal(ARENA1_VA),
            "projectile move-id fetch",
        ),
        (
            FETCH_SITES[1],
            FETCH_RETAIL,
            m::jal(ARENA1_VA),
            "damage move-id fetch",
        ),
    ];
    for (va, retail, hooked, what) in sites {
        let (at, word) = word_at(va)?;
        if word == hooked {
            continue;
        }
        if word != retail {
            bail!("{what} at {va:#010x}: expected {retail:#010x}, found {word:#010x}");
        }
        patcher.patch_prot_entry(BATTLE_OVERLAY_PROT, at as u64, &hooked.to_le_bytes())?;
        changed = true;
    }
    Ok(changed)
}

/// Replace the retail `jal FUN_8004E2F0` at `va` in SCUS with a `jal target`.
fn redirect_scus_call(patcher: &mut DiscPatcher, va: u32, target: u32) -> Result<bool> {
    let scus = patcher
        .read_named_file(SCUS_NAME)
        .context("read SCUS_942.54")?;
    let off = legaia_game_tables::item_names::file_offset_for_va(&scus, va)
        .with_context(|| format!("resolve the file offset of {va:#010x}"))?;
    let word = u32::from_le_bytes(scus[off..off + 4].try_into()?);
    let (retail, hooked) = (m::jal(RANGE_FN_VA), m::jal(target));
    if word == hooked {
        return Ok(false);
    }
    if word != retail {
        bail!("range call at {va:#010x}: expected {retail:#010x}, found {word:#010x}");
    }
    patcher.patch_named_file(SCUS_NAME, off as u64, &hooked.to_le_bytes())?;
    Ok(true)
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

#[cfg(test)]
mod tests {
    use super::*;
    use legaia_code_hooks::mips_sim::Cpu;

    const ACTOR: u32 = 0x8010_0000;
    const CTX: u32 = 0x8011_0000;
    const RETURN_VA: u32 = 0x801E_3200;
    const STACK: u32 = 0x801F_F000;
    const CASTS: [CastArt; 2] = [
        CastArt {
            constant: 0x27,
            spell: 0x2A,
        },
        CastArt {
            constant: 0x30,
            spell: 0x61,
        },
    ];

    fn begin(seat: u32, queue: &[u8], range: u32) -> (u32, Vec<u8>) {
        let mut cpu = Cpu::new();
        let (image, _) = assemble_begin(&CASTS);
        cpu.load(SCUS_GAP_VA, &image);
        cpu.load_words(
            RANGE_FN_VA,
            &[m::addiu(V0, ZERO, range as u16), m::jr(RA), m::nop()],
        );
        cpu.load(ACTOR + 0x1DF, queue);
        cpu.r[S3 as usize] = ACTOR;
        cpu.r[A0 as usize] = seat;
        cpu.r[SP as usize] = STACK;
        cpu.r[RA as usize] = RETURN_VA;
        cpu.pc = SCUS_GAP_VA;
        cpu.run_until(&[RETURN_VA]);
        assert_eq!(cpu.r[SP as usize], STACK, "stack balanced");
        let q = (0..8).map(|i| cpu.rd8(ACTOR + 0x1DF + i)).collect();
        (cpu.r[V0 as usize], q)
    }

    /// Run the fetch stub: v1 = actor in, v1 = move id out; `v0` must survive.
    fn fetch(actor: u32, category: u8, cursor: u8, queue: &[u8]) -> u32 {
        let mut cpu = Cpu::new();
        let (_, table_va) = assemble_begin(&CASTS);
        let (image, _) = assemble_begin(&CASTS);
        cpu.load(SCUS_GAP_VA, &image);
        cpu.load(ARENA1_VA, &assemble_fetch(table_va));
        cpu.wr32(ACTOR_TABLE_VA, ACTOR);
        cpu.wr32(CTX_PTR_VA, CTX);
        cpu.wr8(CTX + 0x15, cursor);
        cpu.wr8(actor + 0x1DE, category);
        cpu.load(actor + 0x1DF, queue);
        cpu.r[V1 as usize] = actor;
        cpu.r[V0 as usize] = 0x801F_4E64;
        cpu.r[RA as usize] = RETURN_VA;
        cpu.pc = ARENA1_VA;
        cpu.run_until(&[RETURN_VA]);
        assert_eq!(cpu.r[V0 as usize], 0x801F_4E64, "v0 preserved");
        cpu.r[V1 as usize]
    }

    #[test]
    fn cast_art_collapses_to_starter_and_constant_in_range() {
        let (v0, q) = begin(0, &[0x0F, 0x0E, 0x19, 0x27, 0x0D, 0], 900);
        assert_eq!(v0, 0);
        assert_eq!(&q[..3], &[0x19, 0x27, 0]);
        // a newly learned art leads with 0x1A and keeps it
        let (v0, q) = begin(0, &[0x0F, 0x0E, 0x1A, 0x27, 0], 900);
        assert_eq!(v0, 0);
        assert_eq!(&q[..3], &[0x1A, 0x27, 0]);
        // a chain that leads with a swing still collapses to the cast
        let (v0, q) = begin(0, &[0x0D, 0x0F, 0x0E, 0x19, 0x30, 0], 900);
        assert_eq!(v0, 0);
        assert_eq!(&q[..3], &[0x19, 0x30, 0]);
    }

    #[test]
    fn other_arts_and_swings_are_untouched() {
        for queue in [
            &[0x0F, 0x0E, 0x0D, 0][..],
            &[0x0F, 0x19, 0x1F, 0x0E, 0][..],
            &[0x19, 0x1F, 0x19, 0x28, 0][..],
            &[0x19, 0x31, 0][..], // past the table
            &[0x19, 0x05, 0][..], // below the table
        ] {
            let (v0, q) = begin(0, queue, 900);
            assert_eq!(v0, 900, "{queue:x?}");
            assert_eq!(&q[..queue.len()], queue);
        }
    }

    #[test]
    fn only_seat_zero_is_rewritten() {
        for seat in [1, 2, 3, 6] {
            let (v0, q) = begin(seat, &[0x0F, 0x19, 0x27, 0], 77);
            assert_eq!(v0, 77);
            assert_eq!(&q[..4], &[0x0F, 0x19, 0x27, 0]);
        }
    }

    #[test]
    fn fetch_returns_the_spell_for_a_cast_art() {
        // [19 27]: cursor 2 -> the byte played is queue[1] = 0x27 -> spell 0x2A
        assert_eq!(fetch(ACTOR, 3, 2, &[0x19, 0x27, 0]), 0x2A);
        assert_eq!(fetch(ACTOR, 3, 2, &[0x19, 0x30, 0]), 0x61);
    }

    #[test]
    fn fetch_falls_back_to_the_head_byte() {
        // starter still playing (cursor 1): retail head
        assert_eq!(fetch(ACTOR, 3, 1, &[0x19, 0x27, 0]), 0x19);
        // a non-cast art
        assert_eq!(fetch(ACTOR, 3, 2, &[0x19, 0x1F, 0]), 0x19);
        // cursor 0
        assert_eq!(fetch(ACTOR, 3, 0, &[0x0F, 0]), 0x0F);
        // not an Attack (an item whose id equals a cast constant)
        assert_eq!(fetch(ACTOR, 1, 2, &[0x19, 0x27, 0]), 0x19);
        // another actor
        assert_eq!(fetch(ACTOR + 0x1000, 3, 2, &[0x19, 0x27, 0]), 0x19);
    }

    /// Run the snap stub as the commit block's first range call: returns
    /// where it ended up (`SNAP_EXIT_VA` or the real range function).
    fn snap(seat: u32, queue: &[u8]) -> u32 {
        let mut cpu = Cpu::new();
        let (image, table_va) = assemble_begin(&CASTS);
        cpu.load(SCUS_GAP_VA, &image);
        cpu.load(snap_va(), &assemble_snap(table_va));
        cpu.load(ACTOR + 0x1DF, queue);
        cpu.r[S1 as usize] = ACTOR;
        cpu.r[A0 as usize] = seat;
        cpu.r[RA as usize] = 0x8004_BFB0;
        cpu.pc = snap_va();
        cpu.run_until(&[SNAP_EXIT_VA, RANGE_FN_VA])
    }

    #[test]
    fn snap_stub_exits_only_for_a_collapsed_cast() {
        assert_eq!(snap(0, &[0x19, 0x27, 0]), SNAP_EXIT_VA);
        assert_eq!(snap(0, &[0x1A, 0x30, 0]), SNAP_EXIT_VA);
        for (seat, queue) in [
            (1, &[0x19, 0x27, 0][..]),          // another seat
            (0, &[0x19, 0x1F, 0][..]),          // not a cast art
            (0, &[0x19, 0x27, 0x0F][..]),       // not collapsed
            (0, &[0x0F, 0x0E, 0x19, 0x27][..]), // retail queue
            (0, &[0x0F, 0][..]),
            (0, &[0x19, 0x31, 0][..]),
        ] {
            assert_eq!(snap(seat, queue), RANGE_FN_VA, "{seat} {queue:x?}");
        }
    }

    #[test]
    fn stubs_fit_their_arenas() {
        let (image, table_va) = assemble_begin(&CASTS);
        assert!(SCUS_GAP_VA as usize + image.len() <= SCUS_GAP_END_VA as usize);
        assert!(snap_va() as usize + assemble_snap(table_va).len() <= ARENA1_END_VA as usize);
    }
}

#[cfg(test)]
mod dump {
    /// Writes the assembled stubs to `$STUB_DUMP_DIR` for disassembly review.
    #[test]
    #[ignore]
    fn dump_stubs() {
        let Some(dir) = std::env::var_os("STUB_DUMP_DIR") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let (begin, table_va) = super::assemble_begin(&[super::CastArt {
            constant: 0x27,
            spell: 0x2A,
        }]);
        std::fs::write(dir.join("begin.bin"), begin).unwrap();
        std::fs::write(dir.join("fetch.bin"), super::assemble_fetch(table_va)).unwrap();
        std::fs::write(dir.join("snap.bin"), super::assemble_snap(table_va)).unwrap();
    }
}
