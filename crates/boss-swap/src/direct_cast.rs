//! Stage 2: the host's special techniques cast the boss's fireball where the
//! host stands.
//!
//! The fireball lives in direction swing `0xF` (queue byte `0x0F`): its swing
//! record plays the boss's cast clip and the move-power id map sends that byte
//! to the boss's spell record (`route_fireball_move`). That path casts with the
//! projectile, so the techniques reuse it instead of carrying their own clip.
//!
//! A *cast art* is an art whose queue constant is listed in the table below.
//! Its input stays the retail one (Somersault is Up, Down, Up) and the retail
//! tokenizer still turns it into `[arrows.., starter, constant]`. Two hooks
//! change what happens next:
//!
//! 1. **Attack begin** (`FUN_801E295C` state `0x14`, battle-action overlay
//!    PROT 898, base `0x801CE818`). Retail calls `FUN_8004E2F0(attacker,
//!    target)` at `0x801E31F4` and walks in unless the range is zero. The call
//!    goes through a stub that runs the real range function and then, for seat
//!    0 only, scans the queue at `actor[+0x1DF]` for a starter (`0x19`, or
//!    `0x1A` for a newly learned art) followed by a cast-art constant. On a
//!    hit the whole queue becomes the single byte `0x0F` and the stub returns
//!    range `0`: the state stores `0x1E` and the fireball swing plays in
//!    place, with the queue head `0x0F` that the id map resolves. Without a
//!    hit every plain `0x0F` in the queue becomes `0x0A` - record[0] row
//!    `0x0A`, which holds the Up melee the fireball displaced (or `0x0E`, the
//!    Down swing, when that row is not authored) - so a bare Up press stays a
//!    melee and the fireball stays a technique.
//! 2. **Commit-time range snap** (SCUS, `0x8004BFA8`). Every clip the strike
//!    loop commits runs a block at the end of the anim commit that calls the
//!    range function and walks the attacker to the range boundary in a single
//!    frame, which would snap the host next to the target. The `jal` at the
//!    block's first range call goes to a second stub that, when the attacker's
//!    queue is exactly `[0x0F, 0]` (the shape the first hook leaves), jumps to
//!    the block's exit (the function's own epilogue, so the stack frame is
//!    intact) and otherwise tail-calls the real range function. The block's
//!    retreat loop never ends if range is forced to 0 for good, so the range
//!    function itself must not be patched.
//!
//! Register contract (read off the disassembly): at `0x801E31F4` `s3` is the
//! acting actor, `s5` is `ctx + 0x11` and no temporary is live across the
//! call; at `0x8004BFA8` `s1` is the committing actor and `a0` its seat.

use crate::module_cast::{HOOK_END_VA, HOOK_VA};
use anyhow::{Context, Result, bail};
use legaia_code_hooks::arenas::{ARENA1_END_VA, ARENA1_VA, SCUS_GAP_END_VA, SCUS_GAP_VA};
use legaia_code_hooks::mips::{
    self as m, A0, A1, A2, RA, S0, S1, S3, SP, T0, T1, T2, T3, T4, T5, T6, V0, V1, ZERO,
};
use legaia_disc_patch::disc::DiscPatcher;

/// Battle-action overlay and its link base.
const BATTLE_OVERLAY_PROT: usize = 898;
const BATTLE_OVERLAY_BASE: u32 = 0x801C_E818;
/// The range-check call in state `0x14`.
const RANGE_CALL_VA: u32 = 0x801E_31F4;
/// `FUN_8004E2F0(attacker, target)`: the attack range metric.
const RANGE_FN_VA: u32 = 0x8004_E2F0;
/// The range-normalising block of the anim commit in `SCUS_942.54` and its
/// exit (the function epilogue).
/// The battle camera's framing function `FUN_801D5854(seat, case)` and the two
/// calls the strike (`0x1E`) and recovery (`0x1F`) states make with case 7 / 8
/// (the two-shot / the target); case 6 frames the acting actor.
const CAMERA_FN_VA: u32 = 0x801D_5854;
/// `FUN_80035F04(name)`: a string's pixel width. `FUN_801D8DE8(element, mode)`:
/// the HUD element opener.
const NAME_WIDTH_FN_VA: u32 = 0x8003_5F04;
const HUD_ELEMENT_FN_VA: u32 = 0x801D_8DE8;
const CAMERA_SITES: [u32; 2] = [0x801E_36E8, 0x801E_3AC8];
const CAMERA_ACTING_CASE: u16 = 6;
const SNAP_SITE_VA: u32 = 0x8004_BFA8;
const SNAP_EXIT_VA: u32 = 0x8004_C11C;
const SCUS_NAME: &str = "SCUS_942.54";

/// Queue byte of the fireball swing (High / Up).
pub const FIREBALL_BYTE: u8 = 0x0F;
/// What a plain Up press plays as when the host's record[0] row `0x0A` holds
/// the displaced Up melee.
pub const UP_MELEE_BYTE: u8 = 0x0A;
/// What it plays as otherwise: the Down swing.
pub const DOWN_SWING_BYTE: u8 = 0x0E;

/// First art constant and the number of constants the table covers
/// (`0x10..=0x30`: every row of the 33-record art bank).
pub const TABLE_BASE: u8 = 0x10;
pub const TABLE_LEN: usize = 0x21;

/// A pending branch: word index, label, offset encoder.
type Fixup = (usize, usize, fn(i16) -> u32);

/// A tiny assembler: words plus branch labels resolved at the end.
struct Asm {
    w: Vec<u32>,
    fix: Vec<Fixup>,
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
fn bne_t1t2(o: i16) -> u32 {
    m::bne(T1, T2, o)
}
fn bne_t1t5(o: i16) -> u32 {
    m::bne(T1, T5, o)
}
fn beq_t3z(o: i16) -> u32 {
    m::beq(T3, ZERO, o)
}
fn bne_t4z(o: i16) -> u32 {
    m::bne(T4, ZERO, o)
}
fn bne_t0t1(o: i16) -> u32 {
    m::bne(T0, T1, o)
}
fn bne_t2z(o: i16) -> u32 {
    m::bne(T2, ZERO, o)
}

/// The cast-art table: one byte per art constant, non-zero for a cast art.
fn table(constants: &[u8]) -> [u8; TABLE_LEN] {
    let mut t = [0u8; TABLE_LEN];
    for &c in constants {
        let i = usize::from(c.wrapping_sub(TABLE_BASE));
        if i < TABLE_LEN {
            t[i] = 1;
        }
    }
    t
}

/// Assemble the attack-begin stub (SCUS gap) for a cast table at `table_va`.
pub fn assemble_begin(table_va: u32, plain_up: u8, banner: Option<u32>) -> Vec<u8> {
    begin_words(table_va, plain_up, banner)
        .iter()
        .flat_map(|x| x.to_le_bytes())
        .collect()
}

fn begin_words(table_va: u32, plain_up: u8, banner: Option<u32>) -> Vec<u32> {
    let mut a = Asm::new();
    let (ret, scan, next, found, starter, chk) = (
        a.label(),
        a.label(),
        a.label(),
        a.label(),
        a.label(),
        a.label(),
    );
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
    a.op(m::addiu(T5, ZERO, u16::from(FIREBALL_BYTE)));
    a.op(m::addiu(T6, ZERO, u16::from(plain_up)));
    a.bind(scan);
    a.op(m::lbu(T1, T0, 0));
    a.op(m::addiu(T2, ZERO, 0x19)); // (load-delay filler)
    a.br(ret, beq_t1z); // terminator: no cast art
    a.op(m::nop());
    a.br(chk, bne_t1t5);
    a.op(m::nop());
    // A plain Up byte plays as the Down swing. A cast art found further on
    // overwrites the whole queue, so editing in place is harmless.
    a.op(m::sb(T6, T0, 0));
    a.br(next, beq_zz);
    a.op(m::nop());
    a.bind(chk);
    a.br(starter, beq_t1t2); // 0x19: a known art
    a.op(m::addiu(T2, ZERO, 0x1A)); // delay
    a.br(next, bne_t1t2); // not 0x1A (newly learned art) either
    a.op(m::nop());
    a.bind(starter);
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
    a.op(m::sb(T5, S3, 0x1DF)); // t5 = FIREBALL_BYTE
    a.op(m::sb(ZERO, S3, 0x1E0));
    a.op(m::addu(V0, ZERO, ZERO)); // in range
    if let Some(va) = banner {
        // tail-jump: `ra` is the caller's, the banner routine returns v0 = 0
        a.op(m::j(va));
        a.op(m::nop());
    }
    a.bind(ret);
    a.op(m::jr(RA));
    a.op(m::nop());
    a.finish()
}

/// VA of the snap stub: the start of ARENA1.
pub fn snap_va() -> u32 {
    ARENA1_VA
}

/// VA of the cast table: right after the snap stub in ARENA1.
pub fn table_va() -> u32 {
    snap_va() + assemble_snap().len() as u32
}

/// Assemble the commit-time snap stub (`s1` is the committing actor, `a0` its
/// seat).
pub fn assemble_snap() -> Vec<u8> {
    let mut a = Asm::new();
    let real = a.label();
    a.br(real, bne_a0); // only seat 0
    a.op(m::nop());
    a.op(m::lbu(T0, S1, 0x1DF)); // first queue byte
    a.op(m::addiu(T1, ZERO, u16::from(FIREBALL_BYTE)));
    a.op(m::lbu(T2, S1, 0x1E0)); // must be the terminator
    a.op(m::nop());
    a.br(real, bne_t0t1);
    a.op(m::nop());
    a.br(real, bne_t2z);
    a.op(m::nop());
    a.op(m::j(SNAP_EXIT_VA));
    a.op(m::nop());
    a.bind(real);
    a.op(m::j(RANGE_FN_VA));
    a.op(m::nop());
    a.finish().iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// Raise the spell-name banner (`Hyper Wave`) for a cast of `spell`, the way a
/// monster caster's cast begin (state `0x28`) does: the name pointer out of the
/// spell table (`0x800754C8 + spell * 12 + 8`), centred on the screen by its
/// width (`FUN_80035F04`) into the four descriptor slots, then HUD element
/// `0x4C` (`FUN_801D8DE8`). A party caster's cast begin skips that block, and
/// the fireball is a swing, so the banner never came up. Entered by a tail
/// jump from the begin stub; returns `v0 = 0` (in range) to its caller.
pub fn assemble_banner(spell: u8) -> Vec<u8> {
    let mut a = Asm::new();
    a.op(m::addiu(SP, SP, 0xFFE0));
    a.op(m::sw(RA, SP, 0x1C));
    a.op(m::sw(S0, SP, 0x18));
    a.op(m::lui(S0, 0x8007));
    a.op(m::addiu(S0, S0, 0x54C8)); // spell table
    a.op(m::addiu(V1, ZERO, u16::from(spell)));
    a.op(m::sll(V0, V1, 1));
    a.op(m::addu(V0, V0, V1));
    a.op(m::sll(V0, V0, 2)); // * 12
    a.op(m::addu(S0, S0, V0)); // this spell's record
    a.op(m::lw(A0, S0, 8)); // name pointer
    a.op(m::jal(NAME_WIDTH_FN_VA));
    a.op(m::nop());
    a.op(m::sra(V0, V0, 1));
    a.op(m::addiu(V1, ZERO, 0xA0));
    a.op(m::subu(V1, V1, V0)); // centred x
    a.op(m::lui(A2, 0x8007));
    a.op(m::addiu(A2, A2, 0x6C10));
    for off in [0x742u16, 0x73A, 0x72A, 0x722] {
        a.op(m::sh(V1, A2, off));
    }
    a.op(m::lw(V0, S0, 8));
    a.op(m::addiu(A0, ZERO, 0x4C));
    a.op(m::addu(A1, ZERO, ZERO));
    a.op(m::sw(V0, A2, 0x74C));
    a.op(m::jal(HUD_ELEMENT_FN_VA));
    a.op(m::sw(V0, A2, 0x734)); // delay slot, as the retail block
    a.op(m::lw(RA, SP, 0x1C));
    a.op(m::lw(S0, SP, 0x18));
    a.op(m::addu(V0, ZERO, ZERO));
    a.op(m::jr(RA));
    a.op(m::addiu(SP, SP, 0x20));
    a.finish().iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// Where the banner routine sits in the rodata gap, behind the camera stub.
pub const BANNER_VA: u32 = HOOK_VA + 0x60;

/// The camera stub behind the strike and recovery states' framing calls
/// (`a0` = seat, `a1` = the case): for the collapsed fireball swing (seat 0,
/// queue exactly `[0x0F, 0]`) it asks for the acting-actor framing, so the
/// camera stays on the host while it casts instead of travelling to the target
/// before the projectile leaves; any other call goes through unchanged.
pub fn assemble_camera() -> Vec<u8> {
    let mut a = Asm::new();
    let real = a.label();
    a.br(real, bne_a0); // only seat 0
    a.op(m::nop());
    a.op(m::lui(T0, 0x801D));
    a.op(m::lw(T0, T0, 0x9370)); // actor_table[0]
    a.op(m::nop());
    a.op(m::lbu(T1, T0, 0x1DF)); // first queue byte
    a.op(m::addiu(T2, ZERO, u16::from(FIREBALL_BYTE)));
    a.op(m::lbu(T3, T0, 0x1E0)); // must be the terminator
    a.op(m::nop());
    a.br(real, bne_t1t2);
    a.op(m::nop());
    a.br(real, bne_t3z);
    a.op(m::nop());
    a.op(m::addiu(A1, ZERO, CAMERA_ACTING_CASE));
    a.bind(real);
    a.op(m::j(CAMERA_FN_VA));
    a.op(m::nop());
    a.finish().iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn bne_t3z(o: i16) -> u32 {
    m::bne(T3, ZERO, o)
}

/// Write the stubs and redirect the two range calls. Returns `false` when
/// everything is already in place.
pub fn install(
    patcher: &mut DiscPatcher,
    constants: &[u8],
    plain_up: u8,
    banner_spell: u8,
) -> Result<bool> {
    if constants.is_empty() {
        bail!("direct cast needs at least one cast art");
    }
    let begin = assemble_begin(table_va(), plain_up, Some(BANNER_VA));
    if SCUS_GAP_VA + begin.len() as u32 > SCUS_GAP_END_VA {
        bail!("direct-cast stub does not fit the SCUS gap");
    }
    let snap = assemble_snap();
    let tbl = table(constants);
    if table_va() + tbl.len() as u32 > ARENA1_END_VA {
        bail!("direct-cast snap stub and table do not fit ARENA1");
    }
    let mut changed = false;
    changed |= write_scus(patcher, SCUS_GAP_VA, &begin)?;
    changed |= write_scus(patcher, snap_va(), &snap)?;
    changed |= write_scus(patcher, table_va(), &tbl)?;
    changed |= redirect_scus_call(patcher, SNAP_SITE_VA, snap_va())?;
    let camera = assemble_camera();
    if HOOK_VA + camera.len() as u32 > HOOK_END_VA {
        bail!("direct-cast camera stub does not fit its gap");
    }
    changed |= write_scus(patcher, HOOK_VA, &camera)?;
    let banner = assemble_banner(banner_spell);
    if BANNER_VA + banner.len() as u32 > HOOK_END_VA || camera.len() as u32 > BANNER_VA - HOOK_VA {
        bail!("direct-cast banner routine does not fit its gap");
    }
    changed |= write_scus(patcher, BANNER_VA, &banner)?;

    let entry = patcher.read_entry(BATTLE_OVERLAY_PROT)?;
    let at = (RANGE_CALL_VA - BATTLE_OVERLAY_BASE) as usize;
    let word = u32::from_le_bytes(entry[at..at + 4].try_into()?);
    let (retail, hooked) = (m::jal(RANGE_FN_VA), m::jal(SCUS_GAP_VA));
    if word != hooked {
        if word != retail {
            bail!(
                "range call at {RANGE_CALL_VA:#010x}: expected {retail:#010x}, found {word:#010x}"
            );
        }
        patcher.patch_prot_entry(BATTLE_OVERLAY_PROT, at as u64, &hooked.to_le_bytes())?;
        changed = true;
    }
    for site in CAMERA_SITES {
        let at = (site - BATTLE_OVERLAY_BASE) as usize;
        let entry = patcher.read_entry(BATTLE_OVERLAY_PROT)?;
        let word = u32::from_le_bytes(entry[at..at + 4].try_into()?);
        let (retail, hooked) = (m::jal(CAMERA_FN_VA), m::jal(HOOK_VA));
        if word != hooked {
            if word != retail {
                bail!("camera call at {site:#010x}: expected {retail:#010x}, found {word:#010x}");
            }
            patcher.patch_prot_entry(BATTLE_OVERLAY_PROT, at as u64, &hooked.to_le_bytes())?;
            changed = true;
        }
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
    const RETURN_VA: u32 = 0x801E_3200;
    const STACK: u32 = 0x801F_F000;
    const CASTS: [u8; 2] = [0x27, 0x30];

    fn begin(seat: u32, queue: &[u8], range: u32) -> (u32, Vec<u8>) {
        let mut cpu = Cpu::new();
        cpu.load(
            SCUS_GAP_VA,
            &assemble_begin(table_va(), UP_MELEE_BYTE, Some(BANNER_VA)),
        );
        cpu.load(table_va(), &table(&CASTS));
        // stand-in for the banner routine: flags the call, returns in range
        cpu.load_words(
            BANNER_VA,
            &[
                m::lui(T0, 0x8010),
                m::addiu(T1, ZERO, 1),
                m::sb(T1, T0, 0x1000),
                m::addu(V0, ZERO, ZERO),
                m::jr(RA),
                m::nop(),
            ],
        );
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
        let q = (0..0x14).map(|i| cpu.rd8(ACTOR + 0x1DF + i)).collect();
        (cpu.r[V0 as usize], q)
    }

    #[test]
    fn no_stub_reads_a_register_in_its_load_delay() {
        for b in [
            assemble_begin(table_va(), UP_MELEE_BYTE, Some(BANNER_VA)),
            assemble_begin(table_va(), 0x0E, Some(BANNER_VA)),
            assemble_snap(),
        ] {
            let w: Vec<u32> = b
                .chunks(4)
                .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
                .collect();
            assert!(crate::hazard::load_hazards(&w).is_empty());
        }
    }

    #[test]
    fn cast_art_collapses_to_the_fireball_swing_in_range() {
        for queue in [
            &[0x0F, 0x0E, 0x19, 0x27, 0][..],
            &[0x0F, 0x0E, 0x1A, 0x27, 0][..], // newly learned art
            &[0x0D, 0x0F, 0x0E, 0x19, 0x30, 0][..], // chain led by a swing
            &[0x19, 0x1F, 0x0C, 0x19, 0x27, 0][..], // another art first
        ] {
            let (v0, q) = begin(0, queue, 900);
            assert_eq!(v0, 0, "{queue:x?}");
            assert_eq!(&q[..2], &[FIREBALL_BYTE, 0], "{queue:x?}");
        }
    }

    #[test]
    fn the_banner_comes_up_once_for_a_cast_art_and_never_otherwise() {
        let fired = |seat: u32, queue: &[u8]| {
            let mut cpu = Cpu::new();
            cpu.load(
                SCUS_GAP_VA,
                &assemble_begin(table_va(), UP_MELEE_BYTE, Some(BANNER_VA)),
            );
            cpu.load(table_va(), &table(&CASTS));
            cpu.load_words(
                BANNER_VA,
                &[
                    m::lui(T0, 0x8010),
                    m::addiu(T1, ZERO, 1),
                    m::sb(T1, T0, 0x1000),
                    m::addu(V0, ZERO, ZERO),
                    m::jr(RA),
                    m::nop(),
                ],
            );
            cpu.load_words(RANGE_FN_VA, &[m::addiu(V0, ZERO, 900), m::jr(RA), m::nop()]);
            cpu.load(ACTOR + 0x1DF, queue);
            cpu.r[S3 as usize] = ACTOR;
            cpu.r[A0 as usize] = seat;
            cpu.r[SP as usize] = STACK;
            cpu.r[RA as usize] = RETURN_VA;
            cpu.pc = SCUS_GAP_VA;
            cpu.run_until(&[RETURN_VA]);
            (cpu.rd8(0x8010_1000) == 1, cpu.r[V0 as usize])
        };
        assert_eq!(fired(0, &[0x0F, 0x0E, 0x19, 0x27, 0]), (true, 0));
        assert_eq!(fired(0, &[0x0F, 0x0C, 0]), (false, 900), "a plain Up");
        assert_eq!(
            fired(1, &[0x0F, 0x19, 0x27, 0]),
            (false, 900),
            "not the host"
        );
        assert_eq!(
            fired(0, &[0x19, 0x31, 0x0D, 0]),
            (false, 900),
            "not a cast art"
        );
    }

    #[test]
    fn plain_up_plays_as_the_up_melee_row() {
        let (v0, q) = begin(0, &[0x0F, 0x0C, 0x0F, 0x0D, 0], 900);
        assert_eq!(v0, 900, "range untouched");
        assert_eq!(&q[..5], &[0x0A, 0x0C, 0x0A, 0x0D, 0]);
        // other arts keep their shape, only the Up bytes change
        let (v0, q) = begin(0, &[0x0F, 0x19, 0x1F, 0], 900);
        assert_eq!(v0, 900);
        assert_eq!(&q[..4], &[0x0A, 0x19, 0x1F, 0]);
    }

    #[test]
    fn untouched_queues_and_other_seats() {
        let (v0, q) = begin(0, &[0x19, 0x31, 0x0D, 0], 5);
        assert_eq!(v0, 5);
        assert_eq!(&q[..4], &[0x19, 0x31, 0x0D, 0]); // past the table
        for seat in [1, 2, 3, 6] {
            let (v0, q) = begin(seat, &[0x0F, 0x19, 0x27, 0], 77);
            assert_eq!(v0, 77);
            assert_eq!(&q[..4], &[0x0F, 0x19, 0x27, 0], "seat {seat}");
        }
        let (v0, q) = begin(0, &[0], 55);
        assert_eq!((v0, q[0]), (55, 0));
    }

    /// Run the snap stub as the commit block's first range call: returns
    /// where it ended up (`SNAP_EXIT_VA` or the real range function).
    fn snap(seat: u32, queue: &[u8]) -> u32 {
        let mut cpu = Cpu::new();
        cpu.load(snap_va(), &assemble_snap());
        cpu.load(ACTOR + 0x1DF, queue);
        cpu.r[S1 as usize] = ACTOR;
        cpu.r[A0 as usize] = seat;
        cpu.r[RA as usize] = 0x8004_BFB0;
        cpu.pc = snap_va();
        cpu.run_until(&[SNAP_EXIT_VA, RANGE_FN_VA])
    }

    #[test]
    fn snap_stub_exits_only_for_the_collapsed_fireball() {
        assert_eq!(snap(0, &[0x0F, 0]), SNAP_EXIT_VA);
        for (seat, queue) in [
            (1, &[0x0F, 0][..]),
            (0, &[0x0E, 0][..]),
            (0, &[0x0F, 0x0E][..]),
            (0, &[0x0F, 0x19, 0x27, 0][..]),
            (0, &[0][..]),
        ] {
            assert_eq!(snap(seat, queue), RANGE_FN_VA, "{seat} {queue:x?}");
        }
    }

    /// Run the camera stub: the case it hands `FUN_801D5854` for a call made
    /// with case 7 by `seat` while the host's queue is `queue`.
    fn camera(seat: u32, queue: &[u8]) -> u32 {
        let mut cpu = Cpu::new();
        cpu.load(HOOK_VA, &assemble_camera());
        cpu.wr32(0x801C_9370, ACTOR);
        cpu.load(ACTOR + 0x1DF, queue);
        cpu.r[A0 as usize] = seat;
        cpu.r[A1 as usize] = 7;
        cpu.r[RA as usize] = 0x801E_36F0;
        cpu.pc = HOOK_VA;
        assert_eq!(cpu.run_until(&[CAMERA_FN_VA]), CAMERA_FN_VA);
        assert_eq!(cpu.r[A0 as usize], seat, "seat untouched");
        cpu.r[A1 as usize]
    }

    #[test]
    fn camera_frames_the_caster_only_for_the_collapsed_fireball() {
        assert_eq!(camera(0, &[0x0F, 0]), u32::from(CAMERA_ACTING_CASE));
        for (seat, queue) in [
            (1, &[0x0F, 0][..]),
            (0, &[0x0E, 0][..]),
            (0, &[0x0A, 0][..]),
            (0, &[0x0F, 0x0E][..]),
            (0, &[0x0F, 0x19, 0x27, 0][..]),
        ] {
            assert_eq!(camera(seat, queue), 7, "{seat} {queue:x?}");
        }
    }

    #[test]
    fn the_banner_names_the_spell_and_returns_in_range() {
        let mut cpu = Cpu::new();
        cpu.load(BANNER_VA, &assemble_banner(0x2A));
        // the spell table row 0x2A: name pointer at +8
        let row = 0x8007_54C8 + 0x2A * 12;
        cpu.wr32(row + 8, 0x8009_1234);
        // FUN_80035F04: width 40; FUN_801D8DE8: records its element and mode
        cpu.load_words(
            NAME_WIDTH_FN_VA,
            &[m::addiu(V0, ZERO, 40), m::jr(RA), m::nop()],
        );
        cpu.load_words(
            HUD_ELEMENT_FN_VA,
            &[
                m::lui(T0, 0x8010),
                m::sw(A0, T0, 0),
                m::sw(A1, T0, 4),
                m::jr(RA),
                m::nop(),
            ],
        );
        cpu.r[S0 as usize] = 0xCAFE;
        cpu.r[SP as usize] = STACK;
        cpu.r[RA as usize] = RETURN_VA;
        cpu.pc = BANNER_VA;
        cpu.run_until(&[RETURN_VA]);
        assert_eq!(cpu.r[V0 as usize], 0, "in range");
        assert_eq!(cpu.r[SP as usize], STACK, "stack balanced");
        assert_eq!(cpu.r[S0 as usize], 0xCAFE, "s0 preserved");
        assert_eq!((cpu.rd32(0x8010_0000), cpu.rd32(0x8010_0004)), (0x4C, 0));
        // centred on x = 160: 160 - 40 / 2 = 140 in the four slots
        for off in [0x742u32, 0x73A, 0x72A, 0x722] {
            assert_eq!(cpu.rd32(0x8007_6C10 + off) & 0xFFFF, 140, "{off:#x}");
        }
        assert_eq!(cpu.rd32(0x8007_6C10 + 0x734), 0x8009_1234);
        assert_eq!(cpu.rd32(0x8007_6C10 + 0x74C), 0x8009_1234);
        let w: Vec<u32> = assemble_banner(0x2A)
            .chunks(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        assert!(crate::hazard::load_hazards(&w).is_empty());
        assert!(BANNER_VA as usize + w.len() * 4 <= HOOK_END_VA as usize);
    }

    #[test]
    fn camera_stub_has_no_load_hazard_and_fits() {
        let b = assemble_camera();
        let w: Vec<u32> = b
            .chunks(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        assert!(crate::hazard::load_hazards(&w).is_empty());
        assert!(HOOK_VA as usize + b.len() <= HOOK_END_VA as usize);
    }

    #[test]
    fn stubs_fit_their_arenas() {
        let image = assemble_begin(table_va(), UP_MELEE_BYTE, Some(BANNER_VA));
        assert!(SCUS_GAP_VA as usize + image.len() <= SCUS_GAP_END_VA as usize);
        assert!(table_va() as usize + TABLE_LEN <= ARENA1_END_VA as usize);
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
        std::fs::write(
            dir.join("begin.bin"),
            super::assemble_begin(
                super::table_va(),
                super::UP_MELEE_BYTE,
                Some(super::BANNER_VA),
            ),
        )
        .unwrap();
        std::fs::write(dir.join("snap.bin"), super::assemble_snap()).unwrap();
    }
}
