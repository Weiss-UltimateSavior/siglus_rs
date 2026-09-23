# Siglus PS Vita port

The Vita player uses the shared Rust Siglus VM, resource decoders, text code,
and Kira mixer. Its platform entry point uses VitaSDK bindings for input,
audio, and memory readings. The renderer uses the published vitaGL SDK library
to submit ordinary sprites as GPU textured quads. Complex effects currently
use the shared CPU compositor and upload its output as a GPU texture. No SDK or
third-party source is copied into this repository.

## Build

Install VitaSDK, `cargo-vita`, Rust nightly, and the VitaSDK `vitaGL` package.
From this repository:

```sh
cd platform/vita/player
export VITASDK=/usr/local/vitasdk
cargo +nightly vita build vpk -- --release
```

The VPK is written to
`platform/vita/player/target/armv7-sony-vita-newlibeabihf/release/siglus_vita_player.vpk`.
Install it on a Vita homebrew environment and place your own game files under
`ux0:data/siglus_rs/game/`, including `Scene.pck`, `Gameexe.dat`, and any
required `key.toml`. Saves and logs use `ux0:data/siglus_rs/`. The player log
is `ux0:data/siglus_rs/vita-player.log`.

vitaGL requires a working `libshacccg.suprx` shader compiler at
`ur0:data/libshacccg.suprx`. The player checks for it at startup and reports
an error in the player log when it cannot load. Supply this runtime component
from your own Vita/PSM installation. It is not bundled with the VPK. For
Vita3K, also install its firmware font package before judging text rendering.

The standalone `bootstrap/` probe can be built in the same way. It checks
display and storage access without loading the engine or game assets.

## Input and memory

Cross confirms, Circle cancels, the D-pad navigates, and the front touch panel
maps to the letterboxed game viewport. The player accepts game logical sizes
up to 1280×720 and presents at 960×544.

vitaGL starts with two scanout buffers, a 4 MiB circular pool, a 32 MiB RAM
pool, and a 16 MiB CDRAM pool. The sprite texture cache evicts old entries at
16 MiB. On the tested Vita3K setup, GPU initialization changed reported free
user memory from 248,512,512 to 214,958,080 bytes and free CDRAM from
117,440,512 to 100,663,296 bytes. These figures describe the emulator's
reported free pools, not total application peak usage.

The RewriteHF `Scene.pck` used for bring-up is about 11 MiB compressed and
43 MiB rebuilt. The rebuilt pack now remains in one `Arc<Vec<u8>>` allocation
shared by the initial scene and the VM cache. Startup no longer copies the
whole pack into an `Arc<[u8]>` or reloads it during the first scene restart.
The CPU RGBA frame is allocated only when a complex effect needs it; a
1280×720 RGBA frame uses about 3.5 MiB. Audio output uses 512 stereo frames
and a 128 KiB worker stack. The newlib heap cap is 160 MiB. Vita movie streams
keep one presented MPEG/OMV frame and one queued frame. MPEG, OMV, and WMV convert
directly to no more than 960×544 RGBA pixels; for the RewriteHF 1280×720
opening this reduces each frame from 3.69 MiB to 2.07 MiB. OMV loop-head
frame caching is disabled on Vita; indexed seeking handles loop restarts.
Movie streams that a scene has not polled for two seconds release their decoder
and frame queue.
Not all image, audio, mesh, and text caches have a measured total cap yet.
The periodic log counts the rebuilt pack, live decoded RGBA images, GPU
textures, and movie frames/PCM separately so growth can be tracked during
long play sessions.

For emulator diagnosis only, creating
`ux0:data/siglus_rs/disable-audio` skips Vita audio-port initialization.
Normal Vita builds start audio by default. When output is disabled, the player
still advances Kira's mixer with silent 16 ms buffers so movie audio remains
a working clock. Remove this file to test audio.

## Validation status

The player links and packages with VitaSDK and `cargo-vita`. On Vita3K with
RewriteHF and `libshacccg.suprx` installed, it parses `Gameexe.dat`, rebuilds
`Scene.pck`, initializes the VM, and displays the Key opening animation through
the GPU at roughly 57–60 FPS. A diagnostic run completed the roughly 100-second
`op00.mpg` opening and returned to the script, passing 8400 player frames.
The movie timer and picture advance with the silent mixer clock. The reduced
MPEG frame path held one 2.07 MiB RGBA frame at frame 3000. After the opening,
the observed heap arena reached about 155 MiB, close to the 160 MiB cap;
further scene and hardware testing is needed. Vita3K still reports a missing
font package. Its SDL audio subsystem
did not initialize and its `sceAudioOutOpenPort` implementation crashed, so
the frame trace used `disable-audio`. No physical Vita run or complete
RewriteHF playthrough has been verified. The port remains experimental; see
[ROADMAP.md](ROADMAP.md).
