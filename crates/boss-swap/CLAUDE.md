# CLAUDE.md - boss-swap

Guidance for Claude Code when working in `crates/boss-swap` and `crates/party-swap/src/playerize/boss.rs`. The repo-root `CLAUDE.md` rules apply (no Sony bytes, workspace deps, fmt + clippy, present-tense docs).

## Goal

Play as a monster in battle. `boss-swap build` writes a PPF that replaces **Vahn's battle model only** (field model untouched) with a monster's model, motions, textures and (planned) stats and voices. The user applies the PPF to their own ISO in DuckStation and reports back. No emulator runs in the cloud, so everything below is verified offline by rendering and by the disc's own parsers.

Staged plan:

1. Model, own motions, one art that fires the monster's spell: **done**.
2. Special techniques are the boss's fireball, cast in place: **built** (`direct_cast`). Somersault (Up, Down, Up) casts the fireball swing with no walk and no leading arrows; a bare Up plays its own melee (record[0] row `0x0A`); every other input plays as before. More cast arts (Cort's spells) and a magic-window entry are the open follow-ups.
3. Monster stats and voices: **open**.
4. More monsters (Songi `#76` "Songi (1st)" is the reference; `#136` and other bosses next): **open**.

## Commands

```bash
cargo build --release -p legaia-boss-swap
./target/release/boss-swap build  "<disc.bin>" out.ppf [--monster N] [--scale F] [--no-fireball] [--fireball-art NAME] [--cast-art NAME] [--no-direct-cast] [--no-fireball-swing] [--no-up-melee] [--keep-voices] [--keep-support-anims] [--free-fireball]
./target/release/boss-swap render "<disc.bin>" out_dir [--frames K] [--cell N] [--only ids] [--retail] [+build flags]   # PNG strip per clip
./target/release/boss-swap monster "<disc.bin>" out.png ID [--only a,b]                                                  # unmodified monster
```

The PPF is diffed against an in-memory copy; the ISO is never modified. Defaults: monster 76, scale 0.8, fireball in swing `0xF` with the direct-cast hooks on and Somersault as the cast art, host voices muted, calm support clips, fireball keeps the monster spell's damage.

## How the swap works

- **Player file** (PROT 863, Vahn): `record[0]` holds 12 action slots (0 idle, 1 walk, 2/3 flinch, 4 knockdown, 5 get-up, 6 a stub, 7, 8, 9, 0xA the Up melee, 0xB block; 0xA is a header-only stub in retail), then the art bank (`+0x58`, stride `0xD0`, entry at `+0x24`, inline name `+0x10`, stream source byte `+0x0A`). Equipment sections carry swing records (slots `0xC..=0xF`). Member allocation limit `0x19000`; overflow goes to an annex.
- **Slot 9 is shared**: the commit clip byte `actor[+0x1E7]` is `9` for an item and for a spell, and the defeat pose is also `9` (`battle-action-helpers.md`). Spirit stages `0x10`, which is art-bank record 0 and streams main ME entry 0. So one clip serves item, spell and defeat; the Spirit clip must run at least 59 frames (the stream is addressed to row 58).
- **Arts stream from `readef.DAT`** (PROT 894): per-character main and base `"ME"` archives. Main entries: 0 Spirit clip, 1..=4 single-hit moves, 5 walk, 6 fireball, 7 the multi-hit move, the rest one-frame stubs. Base entries carry the victory composite.
- **Clip heads** (`apply_clip_head`): power run `+0x00..03`, root speed `+0x0C`, hit-event frames `+0x10..13`, effect script `+0x14..53` (8 x 8 bytes; effect `&0x7F == 0x7F` is the terminator that installs the move-power record), sound cues `+0x54..73` (`[u16 frame][u16 id]`; ids `>= 0x100` are party XA voice ids and are stripped), rate `+0x78`, loop `+0x84..86`.
- **Fireball** (swing `0xF`): the cast clip (tag `0x23`) is written into swing record `0xF` (High, section 4 `k=1`) with the charge hold `[10, 10, 11]`; queue byte `0x0F` is also the move id `route_fireball_move` maps to the monster's spell record in PROT 898 (map at file offset `0x2664B`, table `0x26744`, 26-byte records), so one `0x0F` at the queue head is the whole cast with its projectile. `--free-fireball` copies the record into spare record 22 with zero power so only the swing's hit damages. `--fireball-art NAME` is the stage-1 variant that clones the cast clip onto an art record instead (the actor walks up first; turns the swing and the hooks off).
- **Up melee row** (`UP_MELEE_SLOT`, `boss.rs`): the High swing's melee clip (Songi's fourth single-hit move) is built with `swing_entry` from the host's own High swing record (power run and rules) and written as record[0] action row `0x0A`, retail's header-only placeholder. A queue byte `0x0A` commits it through the party arm of the anim commit (`*(table + idx*4)`, the same way a cast module stages row `0x0A` on a player caster, see `cast_stage.rs`). Id `0x0A` is unmapped in the move-power id map, so its hits use the swing's power run. `tests/direct_cast_real.rs` checks the row is authored.
- **Why a swing and not an art clip**: the art-carried cast (starter, constant, spell id returned by a patched move-id fetch) was built and dropped. The swing path is the one confirmed on hardware with a working projectile, and the move-power lookup reads the fixed queue head `actor[+0x1DF]`, which a swing byte at the head satisfies with a plain map entry.
- **Direct cast hooks** (`direct_cast.rs`, one word in PROT 898, one in SCUS, two stubs and a table). A *cast art* is an art whose constant is in the table (Somersault = bank row 23 = `0x27`); the retail tokenizer still turns its input into `[arrows.., starter, constant]`. (1) The `jal FUN_8004E2F0` range call of state `0x14` (`0x801E31F4`) runs a stub in the SCUS gap (`0x80077728`): for seat 0 it scans `actor[+0x1DF..]` for a starter (`0x19`, or `0x1A` for a newly learned art) + a table constant, rewrites the whole queue to the single byte `0x0F` and returns range 0 (state stores `0x1E`, the swing plays in place); without a hit every plain `0x0F` becomes `0x0A`, so a bare Up press plays the Up melee the fireball displaced (`--no-up-melee`: `0x0E`, the Down swing). (2) The anim commit in SCUS ends with a block (`0x8004BF7C..0x8004C11C`) that, in state `0x1E` for the attacker, calls `FUN_8004E2F0` at `0x8004BFA8` and walks the attacker to the range boundary in one frame (forward when out of range, backward when inside): forcing range 0 at state `0x14` alone teleported the host next to the target when the first clip committed (seen on video). The `jal` at `0x8004BFA8` goes to an ARENA1 stub that jumps to the block's exit (`0x8004C11C`, the function epilogue) when the attacker's queue is exactly `[0x0F, 0]` and tail-calls the real range function otherwise. The block's retreat loop never ends if range is forced to 0 for good, so the range function itself must not be patched. `tests/direct_cast_real.rs` (`LEGAIA_DISC_BIN`) checks the words land and survive the PPF; the MIPS-sim tests cover both stubs.
- **Calm support clips** (`add_calm_clips`): slot 9 and Spirit play the boss idle (cycled to 59 frames for Spirit) with an idle head, so no hit event, effect or sound rides along.
- **Textures**: `relayout_to_band` clusters UV islands per source palette (`face_palette = cba & 0x3F`); the downscale ladder `[1, 2, 4]` is tried until the member limit holds.
- **Voices**: Vahn's lines live in XA files and are muted with `DiscPatcher::silence_xa_file` / `silence_xa_channels` (`XA1`, `XA2`, `XA27` whole; `XA30` channel 0; `XA21` channels 2, 3).

## Monster clip tags (Songi `#76`)

0 idle, 1 walk, 2/3 flinch, 4 knockdown, 5 get-up, `0xB` block, `0xE` single-hit moves (four), `0xD` multi-hit (3 events), `0x22` victory, `0x23` cast. Spell id `0x2A` Hyper Wave (class `0x14`), move-power record 20.

## Known behaviour and open problems

- A killing fireball makes one enemy vanish instead of collapsing; another enemy collapsed normally. Attack-specific, not yet explained.
- A starter is `0x19` for a known art and `0x1A` for a newly learned one; the begin stub matches both.
- A chain that contains a cast art collapses to the cast alone: the swings around it are dropped. The cast costs the art's AP, and the ring still accepts a longer input.
- Video of the art-carried build showed the host snapped next to the enemy when the starter committed (the commit-time snap above). The swing-based build removes the art path; the snap stub is kept because the swing commit runs the same block.
- The swing fireball cast from range with the snap stub is confirmed on hardware. The Up melee row (queue byte `0x0A`) has never been staged on hardware: if a plain Up freezes or does nothing, `--no-up-melee` falls back to the Down swing.
- Because slot 9 is shared, a defeated host also shows the idle.
- Songi's own voices are not played yet; the Delilas voice route (`delilas-party`, `delilas_xa_voice`) is the precedent for filling XA channels.
- A save state replays the RAM of the disc that booted it: test every PPF from a cold boot.

## Verifying changes

Render before delivering (`boss-swap render`, read the PNG strips): idle, walk, flinch, knockdown, block, swings, every art strip. Hands and texture collisions were the recurring visual bug. Then `cargo fmt --all`, `cargo clippy --all-targets -p legaia-boss-swap -p legaia-party-swap -- -D warnings`.
