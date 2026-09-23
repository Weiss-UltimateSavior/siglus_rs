# Siglus PS Vita port

The Vita port reuses Siglus scene/VM and resource code. `bootstrap/` is an
isolated device probe. `player/` is an early engine entry point, using the
shared software renderer and Kira mixer. Both need a VitaSDK build and device
validation before gameplay can be claimed.

## Bootstrap build

Install VitaSDK and `cargo-vita`, set `VITASDK`, and build from the bootstrap
directory with a current nightly Rust toolchain:

```sh
cd platform/vita/bootstrap
export VITASDK=/opt/vitasdk
cargo +nightly vita build vpk -- --release
```

Install the resulting VPK on a Vita homebrew environment. A colored screen
means the display buffer was accepted; press START to exit. The program writes
`ux0:data/siglus_rs/vita-bootstrap.log`, including free user/CDRAM memory
before and after display allocation. If
`ux0:data/siglus_rs/bootstrap.txt` exists, the log records its readable byte
count. Supply this file to check storage access.

The probe reserves one 960×544 RGBA display buffer. Its 2,088,960 bytes are
rounded to one 2,097,152-byte CDRAM block. It does not decode game assets.

## Engine player

With VitaSDK and `cargo-vita` installed:

```sh
cd platform/vita/player
export VITASDK=/opt/vitasdk
cargo +nightly vita build vpk -- --release
```

Install the generated VPK and put your own game files under
`ux0:data/siglus_rs/game/`, including `Scene.pck`. Errors and periodic free
user/CDRAM memory readings go to `ux0:data/siglus_rs/vita-player.log`.
Cross confirms, Circle cancels, the D-pad navigates, and the front touch panel
maps to the game viewport. The player uses the game's `SCREEN_SIZE` up to
1280×720, then letterboxes it into 960×544.

The fixed scanout block is 2 MiB. The CPU RGBA frame costs up to about 3.5
MiB at 1280×720; the 3D depth buffer is allocated only when a mesh is drawn
and may add another 3.5 MiB. Audio output holds 512 stereo frames and a
128 KiB worker stack. Vita movie queue counts are lower than desktop counts,
and decoded movie caches are released when playback stops. Active video/audio
data, mesh textures, and other runtime caches do not yet have a reliable total
memory limit. These figures exclude the VM and game resources; inspect the
periodic device log and movie-byte counters before extending the budget.

The Vita entry points use published `vitasdk-sys` bindings and contain no
copied SDK or third-party source. SDK calls stay in these platform entry
points. The shared VM, script, image, and audio logic remain Rust code in
`crates/`. Switch-specific local `ogg` and `lewton` patches are excluded from
the Vita dependency path.

## Current status

The bootstrap and player host stubs pass `cargo check`. The player and engine
also pass a Rust target type check with nightly `-Zbuild-std` and `DOCS_RS=1`,
which bypasses the `vitasdk-sys` build script's SDK path check. The machine
used for this change has no VitaSDK or `cargo-vita`; no VPK has been linked,
packaged, or run on hardware. Suspend/resume, device audio behavior, video
budgets, and visual parity still need validation. See [ROADMAP.md](ROADMAP.md).
