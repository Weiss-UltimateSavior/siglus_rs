# Siglus PS Vita port

The Vita player uses the shared Rust Siglus VM, resource decoders, text code,
and Kira mixer. Its platform entry point uses VitaSDK bindings for input,
audio, and memory readings. The renderer uses the VitaSDK `libvita2d` package
(GXM with precompiled shaders, linked statically) to submit ordinary sprites
as GPU textured quads. Complex effects use the shared CPU compositor and upload
its output as a GPU texture. The VPK needs no runtime shader compiler or other
extra module: only firmware modules are imported. No SDK or third-party source
is copied into this repository.

## Build

Install VitaSDK, `cargo-vita`, Rust nightly, and the VitaSDK `libvita2d`
package (`vdpm install libvita2d`).
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

For Vita3K, install its firmware font package before judging text rendering.

The standalone `bootstrap/` probe can be built in the same way. It checks
display and storage access without loading the engine or game assets.

## Input and memory

Cross confirms, Circle cancels, the D-pad navigates, and the front touch panel
maps to the letterboxed game viewport. The player accepts game logical sizes
up to 1920×1080 (GameData) and presents at 960×544.

vita2d keeps its display buffers in CDRAM. Each sprite texture is its own
memory block: 256 KiB or larger in CDRAM, smaller ones (text, icons) in
uncached user memory, so they do not each occupy a 256 KiB CDRAM unit. The
sprite texture cache evicts old entries at 80 MiB (a 1080p full-screen layer
is 8 MiB); a replaced texture is freed four frames later, because Vita3K's
renderer can still read it after `sceGxmFinish`. vita2d does not initialize
a new texture's render-target and depth fields, which `vita2d_free_texture`
frees when non-zero; the player clears them, or a free could release an
unrelated memory block (the newlib heap included).

Sprites are drawn as textured quads with the normal, add, subtract, multiply
and screen blend modes; the last four use extra fragment programs built from
vita2d's own precompiled tint shader (multiply and screen take premultiplied
textures). Overlay, masks, tone curves, meshes, emotes and position-dependent
effects fall back to the software compositor. Wipes are drawn as a cross-fade
of the next screen over the current one: the desktop renderer's mask and
pattern wipes need shaders vita2d does not have.

The RewriteHF `Scene.pck` used for bring-up is about 11 MiB compressed and
43 MiB rebuilt. The engine no longer rebuilds it: the decrypted, still
compressed pack stays in memory and a scene is decompressed when it is first
used, shared while any stream holds it (`ScenePck::load_lazy`). Fonts are
resolved once per requested face, and a font file is read once per process
and shared by every face and name that uses it (GameData requests one 22 MiB
TTC under many names; each request kept its own copy until the heap ran
out). The embedded default font is parsed in place from the executable.
Plain OGG sound effects of 256 KiB or more stream instead of being decoded
whole (a long ambience loop decoded to 15 MiB). `siglus_scene_vm/examples/memory_probe.rs` runs a scene headlessly
with a counting allocator to measure live and peak heap use and to trace
large allocations (`MEMORY_PROBE_BIG=bytes`). On a RewriteHF route it went
from about 105 MiB live to about 55 MiB with these changes.
The CPU RGBA frame is allocated only when a complex effect needs it; a
1280×720 RGBA frame uses about 3.5 MiB. Audio output uses 512 stereo frames
and a 128 KiB worker stack. The newlib heap is 320 MiB, reserved at startup
(extended memory mode). With 256 MiB about 100 MiB of user memory stayed
unused beside it while GameData's 1080p title menu ran the heap out. If an
allocation still fails, the player log records its size, the heap figures and
the last VM status before the abort. Vita movie streams
keep one presented MPEG/OMV frame and one queued frame. MPEG, OMV, and WMV convert
directly to no more than 960×544 RGBA pixels; for the RewriteHF 1280×720
opening this reduces each frame from 3.69 MiB to 2.07 MiB. OMV loop-head
frame caching is disabled on Vita; indexed seeking handles loop restarts.
An OMV frame is converted straight from the Theora decoder's planes. The
decoder keeps three reference frames, as libtheora's does (it had six), and
no copy of the output frame; a 1920×1440 4:4:4 RGBA OMV frame is 8.6 MiB, and
GameData's title menu plays four such streams at once.
Movie streams that a scene has not polled for 120 engine frames release their
decoder and frame queue. (A wall-clock limit evicted the movie playing when one
frame stalled, and the restart then sat on its first frame.) A restarted MPEG
stream seeks to the GOP before its time, handing the decoder the file's only
sequence header first, and without the audio track takes the duration from the
last video timestamp. Movies decoded smaller than their size (the 960×544 cap)
are drawn stretched to the video's own size, so object movies keep their
position. A movie's texture is rewritten in place for each new frame instead of
being freed and reallocated. `siglus_scene_vm/examples/movie_probe.rs` plays a
movie on a simulated clock (`MOVIE_PROBE_START=ms` to start mid-movie). NWA music is decoded while it plays from the file (one
compressed unit in memory); decoding a whole track up front needed up to
~100 MiB for RewriteHF's longest BGM and ran the heap out of memory at the
title screen.

Global data (flags such as "opening seen", read text, CG and BGM tables) is
written a few seconds after it changes, since a Vita app is normally closed
without an exit the engine sees. RewriteHF only lets the opening be skipped
after it has been watched once, which it records in a global flag.

The player is built without LLVM's SLP vectorizer (`profile.*.rustflags` in
`player/Cargo.toml`): it combined 64-bit saturating arithmetic into NEON
`vqadd.s64`/`vqsub.s64`, which Vita3K cannot execute, breaking input hit
tests and the VM's frame bookkeeping in the emulator.
Not all image, audio, mesh, and text caches have a measured total cap yet.
The periodic log counts the rebuilt pack, live decoded RGBA images, GPU
textures, and movie frames/PCM separately so growth can be tracked during
long play sessions.

For emulator diagnosis only, creating
`ux0:data/siglus_rs/disable-audio` skips Vita audio-port initialization.
Normal Vita builds start audio by default. When output is disabled, the player
still advances Kira's mixer with silent 16 ms buffers so movie audio remains
a working clock. Remove this file to test audio.

`ux0:data/siglus_rs/smoke-taps` scripts touches for emulator runs whose
window cannot deliver clicks: one `frame x y` per line, in game coordinates.
The log records the VM state every 120 frames.

## Validation status

The player links and packages with VitaSDK and `cargo-vita`. The vitaGL build
ran on Vita3K with RewriteHF: it parsed `Gameexe.dat`, initialized the VM,
played the Key logo and the roughly 100-second `op00.mpg` opening, and reached
the title screen once BGM streaming was in place; entering a route then ran the
160 MiB heap out of memory, which led to the memory work above. With the
vita2d renderer, RewriteHF's opening, title and the start of the story run at
55–60 fps on Vita3K. GameData (1920×1080) reaches its title menu and prologue
with audio on Vita3K; its particle frame actions (`$$fa_particle`, thousands
of interpreted object operations per frame) still drop the prologue to 17–30
fps, the VM's object-operation path being about 30 times slower than on a
desktop CPU. Earlier Vita3K builds crashed in `sceAudioOutOpenPort`; the
`disable-audio` marker remains for such emulators. No physical Vita run or
complete playthrough has been verified. The port remains
experimental; see [ROADMAP.md](ROADMAP.md).
