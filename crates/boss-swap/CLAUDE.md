# CLAUDE.md - boss-swap

Guidance for Claude Code when working in `crates/boss-swap` and `crates/party-swap/src/playerize/boss.rs`. The repo-root `CLAUDE.md` rules apply (no Sony bytes, workspace deps, fmt + clippy, present-tense docs).

## Goal

Play as a monster in battle. `boss-swap build` writes a PPF that replaces **Vahn's battle model only** (field model untouched) with a monster's model, motions, textures and (planned) stats and voices. The user applies the PPF to their own ISO in DuckStation and reports back. No emulator runs in the cloud, so everything below is verified offline by rendering and by the disc's own parsers.

Staged plan:

1. Model, own motions, one art that fires the monster's spell: **done**.
2. Special techniques are the boss's specials, cast in place: **built, hook confirmed live for the range half** (`direct_cast`). Somersault (Up, Down, Up) is the fireball with no walk, no arrows and no starter pause; every other input plays as before. More cast arts (Cort's spells) and a magic-window entry are the open follow-ups.
3. Monster stats and voices: **open**.
4. More monsters (Songi `#76` "Songi (1st)" is the reference; `#136` and other bosses next): **open**.

## Commands

```bash
cargo build --release -p legaia-boss-swap
./target/release/boss-swap build  "<disc.bin>" out.ppf [--monster N] [--scale F] [--no-fireball] [--fireball-art NAME] [--no-direct-cast] [--fireball-swing] [--keep-voices] [--keep-support-anims] [--free-fireball]
./target/release/boss-swap render "<disc.bin>" out_dir [--frames K] [--cell N] [--only ids] [--retail] [+build flags]   # PNG strip per clip
./target/release/boss-swap monster "<disc.bin>" out.png ID [--only a,b]                                                  # unmodified monster
```

The PPF is diffed against an in-memory copy; the ISO is never modified. Defaults: monster 76, scale 0.8, fireball art "Somersault" with the direct-cast hooks on, host voices muted, calm support clips, fireball keeps the monster spell's damage.

## How the swap works

- **Player file** (PROT 863, Vahn): `record[0]` holds 12 action slots (0 idle, 1 walk, 2/3 flinch, 4 knockdown, 5 get-up, 6 and 0xA stubs, 7, 8, 9, 0xB block), then the art bank (`+0x58`, stride `0xD0`, entry at `+0x24`, inline name `+0x10`, stream source byte `+0x0A`). Equipment sections carry swing records (slots `0xC..=0xF`). Member allocation limit `0x19000`; overflow goes to an annex.
- **Slot 9 is shared**: the commit clip byte `actor[+0x1E7]` is `9` for an item and for a spell, and the defeat pose is also `9` (`battle-action-helpers.md`). Spirit stages `0x10`, which is art-bank record 0 and streams main ME entry 0. So one clip serves item, spell and defeat; the Spirit clip must run at least 59 frames (the stream is addressed to row 58).
- **Arts stream from `readef.DAT`** (PROT 894): per-character main and base `"ME"` archives. Main entries: 0 Spirit clip, 1..=4 single-hit moves, 5 walk, 6 fireball, 7 the multi-hit move, the rest one-frame stubs. Base entries carry the victory composite.
- **Clip heads** (`apply_clip_head`): power run `+0x00..03`, root speed `+0x0C`, hit-event frames `+0x10..13`, effect script `+0x14..53` (8 x 8 bytes; effect `&0x7F == 0x7F` is the terminator that installs the move-power record), sound cues `+0x54..73` (`[u16 frame][u16 id]`; ids `>= 0x100` are party XA voice ids and are stripped), rate `+0x78`, loop `+0x84..86`.
- **Fireball**: the cast clip (tag `0x23`) is cloned onto the "Hyper Wave"-renamed Somersault art (bank row 23, queue constant `0x27`). `--free-fireball` copies the monster's power record into spare record 22 (spare id `0x2C`) with zero power so only the art's hits damage.
- **Fireball swing** (`--fireball-swing`, needs `--no-direct-cast`): the stage-1 variant that writes the cast clip into swing record `0xF` (Up, section 4 `k=1`) with the charge hold `[10, 10, 11]` and maps the swing byte `0x0F` to the spell record. Up alone then casts, with the retail walk-up.
- **Direct cast hooks** (`direct_cast.rs`, three words in PROT 898, one in SCUS, three stubs). (1) The `jal FUN_8004E2F0` range call of state `0x14` (`0x801E31F4`) runs a stub in the SCUS gap (`0x80077728`, cast-id table after it): for seat 0 it scans `actor[+0x1DF..]` for a starter (`0x19`, or `0x1A` for a newly learned art) + a cast-art constant, rewrites the whole queue to `[starter, constant]` and returns range 0 (state stores `0x1E`, the clip plays in place); queues without a cast art are untouched. (2) The two `lbu v1, 0x1DF(v1)` move-id fetches that feed the id map (`0x801DF250` per-frame projectile tick `FUN_801DEA58`, `0x801E187C` damage setup in `FUN_801E09F8`) run a stub in ARENA1 that, for seat 0 on an Attack, reads the byte being played (`actor[+0x1DE + ctx[+0x15]]`) and returns the art's **spell id**, so the boss's own record is used with no map patch. (3) The commit-time range snap: the anim commit in SCUS ends with a block (`0x8004BF7C..0x8004C11C`) that, in state `0x1E` for the attacker, calls `FUN_8004E2F0` at `0x8004BFA8` and then walks the attacker to the range boundary in one frame (forward when out of range, backward when inside). Forcing range 0 only at state `0x14` therefore teleported the host next to the target when the starter committed (seen on video). The `jal` at `0x8004BFA8` goes to an ARENA1 stub that jumps to the block's exit (`0x8004C11C`, the function epilogue) when the attacker's queue is exactly `[starter, cast constant, 0]` and tail-calls the real range function otherwise. The block's retreat loop never ends if range is forced to 0 for good, so the range function itself must not be patched. (4) The art starters (`Starter` bank rows) stream the calm idle (main ME entry 8) so the walk they played does not move the host. `tests/direct_cast_real.rs` (`LEGAIA_DISC_BIN`) checks the words land; the MIPS-sim tests cover both stubs.
- **Calm support clips** (`add_calm_clips`): slot 9 and Spirit play the boss idle (cycled to 59 frames for Spirit) with an idle head, so no hit event, effect or sound rides along.
- **Textures**: `relayout_to_band` clusters UV islands per source palette (`face_palette = cba & 0x3F`); the downscale ladder `[1, 2, 4]` is tried until the member limit holds.
- **Voices**: Vahn's lines live in XA files and are muted with `DiscPatcher::silence_xa_file` / `silence_xa_channels` (`XA1`, `XA2`, `XA27` whole; `XA30` channel 0; `XA21` channels 2, 3).

## Monster clip tags (Songi `#76`)

0 idle, 1 walk, 2/3 flinch, 4 knockdown, 5 get-up, `0xB` block, `0xE` single-hit moves (four), `0xD` multi-hit (3 events), `0x22` victory, `0x23` cast. Spell id `0x2A` Hyper Wave (class `0x14`), move-power record 20.

## Known behaviour and open problems

- A killing fireball makes one enemy vanish instead of collapsing; another enemy collapsed normally. Attack-specific, not yet explained.
- The first build matched only `0x19`: an art the host has not learned yet tokenizes with `0x1A`, the stub missed it and the retail walk-up, swings and a projectile-less art played. Both starters are matched now.
- A chain that contains a cast art collapses to the cast alone: the swings around it are dropped.
- Video of the 2c build showed the host snapped next to the enemy at the starter (the commit-time snap above) and the charge and release playing in melee range, so the projectile could not be judged; the 2d build removes the snap. The Up-swing build never recorded a video, so whether it snapped is unknown.
- The range half is confirmed on hardware (stage-2 first build, Up swing). The art path, the move-id fetch stub and the calm starter have not run yet: if the projectile is missing, suspect the cursor convention (`ctx[+0x15]` is one past the byte playing); `--no-direct-cast` restores the walk-up art.
- Art constants: Somersault is bank row 23 = constant `0x27`.
- Because slot 9 is shared, a defeated host also shows the idle.
- Songi's own voices are not played yet; the Delilas voice route (`delilas-party`, `delilas_xa_voice`) is the precedent for filling XA channels.
- A save state replays the RAM of the disc that booted it: test every PPF from a cold boot.

## Verifying changes

Render before delivering (`boss-swap render`, read the PNG strips): idle, walk, flinch, knockdown, block, swings, every art strip. Hands and texture collisions were the recurring visual bug. Then `cargo fmt --all`, `cargo clippy --all-targets -p legaia-boss-swap -p legaia-party-swap -- -D warnings`.
