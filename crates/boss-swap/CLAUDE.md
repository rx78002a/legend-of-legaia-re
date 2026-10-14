# CLAUDE.md - boss-swap

Guidance for Claude Code when working in `crates/boss-swap` and `crates/party-swap/src/playerize/boss.rs`. The repo-root `CLAUDE.md` rules apply (no Sony bytes, workspace deps, fmt + clippy, present-tense docs).

## Goal

Play as a monster in battle. `boss-swap build` writes a PPF that replaces **Vahn's battle model only** (field model untouched) with a monster's model, motions, textures and (planned) stats and voices. The user applies the PPF to their own ISO in DuckStation and reports back. No emulator runs in the cloud, so everything below is verified offline by rendering and by the disc's own parsers.

Staged plan:

1. Model, own motions, one art that fires the monster's spell: **done**.
2. No combo system: every action chosen through the menu (arts / magic), the fireball cast from range without the melee walk-up: **open**, needs a code hook.
3. Monster stats and voices: **open**.
4. More monsters (Songi `#76` "Songi (1st)" is the reference; `#136` and other bosses next): **open**.

## Commands

```bash
cargo build --release -p legaia-boss-swap
./target/release/boss-swap build  "<disc.bin>" out.ppf [--monster N] [--scale F] [--no-fireball] [--fireball-art NAME] [--keep-voices] [--keep-support-anims] [--free-fireball]
./target/release/boss-swap render "<disc.bin>" out_dir [--frames K] [--cell N] [--only ids] [--retail] [+build flags]   # PNG strip per clip
./target/release/boss-swap monster "<disc.bin>" out.png ID [--only a,b]                                                  # unmodified monster
```

The PPF is diffed against an in-memory copy; the ISO is never modified. Defaults: monster 76, scale 0.8, fireball art "Somersault", host voices muted, calm support clips, fireball keeps the monster spell's damage.

## How the swap works

- **Player file** (PROT 863, Vahn): `record[0]` holds 12 action slots (0 idle, 1 walk, 2/3 flinch, 4 knockdown, 5 get-up, 6 and 0xA stubs, 7, 8, 9, 0xB block), then the art bank (`+0x58`, stride `0xD0`, entry at `+0x24`, inline name `+0x10`, stream source byte `+0x0A`). Equipment sections carry swing records (slots `0xC..=0xF`). Member allocation limit `0x19000`; overflow goes to an annex.
- **Slot 9 is shared**: the commit clip byte `actor[+0x1E7]` is `9` for an item and for a spell, and the defeat pose is also `9` (`battle-action-helpers.md`). Spirit stages `0x10`, which is art-bank record 0 and streams main ME entry 0. So one clip serves item, spell and defeat; the Spirit clip must run at least 59 frames (the stream is addressed to row 58).
- **Arts stream from `readef.DAT`** (PROT 894): per-character main and base `"ME"` archives. Main entries: 0 Spirit clip, 1..=4 single-hit moves, 5 walk, 6 fireball, 7 the multi-hit move, the rest one-frame stubs. Base entries carry the victory composite.
- **Clip heads** (`apply_clip_head`): power run `+0x00..03`, root speed `+0x0C`, hit-event frames `+0x10..13`, effect script `+0x14..53` (8 x 8 bytes; effect `&0x7F == 0x7F` is the terminator that installs the move-power record), sound cues `+0x54..73` (`[u16 frame][u16 id]`; ids `>= 0x100` are party XA voice ids and are stripped), rate `+0x78`, loop `+0x84..86`.
- **Fireball**: the cast clip (tag `0x23`) is cloned onto the "Hyper Wave"-renamed Somersault art; its terminator installs the move-power record mapped to move id `0x0F` (the art's queue id). `route_fireball_move` maps `0x0F` to the monster's spell record in PROT 898 (map at file offset `0x2664B`, table `0x26744`, 26-byte records). `--free-fireball` copies the record into spare record 22 with zero power so only the art's hits damage.
- **Calm support clips** (`add_calm_clips`): slot 9 and Spirit play the boss idle (cycled to 59 frames for Spirit) with an idle head, so no hit event, effect or sound rides along.
- **Textures**: `relayout_to_band` clusters UV islands per source palette (`face_palette = cba & 0x3F`); the downscale ladder `[1, 2, 4]` is tried until the member limit holds.
- **Voices**: Vahn's lines live in XA files and are muted with `DiscPatcher::silence_xa_file` / `silence_xa_channels` (`XA1`, `XA2`, `XA27` whole; `XA30` channel 0; `XA21` channels 2, 3).

## Monster clip tags (Songi `#76`)

0 idle, 1 walk, 2/3 flinch, 4 knockdown, 5 get-up, `0xB` block, `0xE` single-hit moves (four), `0xD` multi-hit (3 events), `0x22` victory, `0x23` cast. Spell id `0x2A` Hyper Wave (class `0x14`), move-power record 20.

## Known behaviour and open problems

- A killing fireball makes one enemy vanish instead of collapsing; another enemy collapsed normally. Attack-specific, not yet explained.
- The fireball is an art, so the actor still walks up to melee range first; casting from range needs a code hook (stage 2) or the magic menu flow.
- Because slot 9 is shared, a defeated host also shows the idle.
- Songi's own voices are not played yet; the Delilas voice route (`delilas-party`, `delilas_xa_voice`) is the precedent for filling XA channels.
- A save state replays the RAM of the disc that booted it: test every PPF from a cold boot.

## Verifying changes

Render before delivering (`boss-swap render`, read the PNG strips): idle, walk, flinch, knockdown, block, swings, every art strip. Hands and texture collisions were the recurring visual bug. Then `cargo fmt --all`, `cargo clippy --all-targets -p legaia-boss-swap -p legaia-party-swap -- -D warnings`.
