# uk2

`uk2` is a format-first implementation of AyPio's UK2 engine, initially
reconstructed from the PC-98 version of *Sorcer Kingdom*.

The crate intentionally does not route UK2 through AVG32: UK2 uses a distinct
MES VM, operand model, recursive status protocol, DLB archive format, and the
older PDT34 graphics family.

Implemented in this initial port:

- `<< UK2 TEXT Ver1.00 >>` MES validation and entry point;
- all 88 two-character opcodes plus the six single-byte commands;
- direct operands, nested expressions, conditions, and null-terminated
  expression lists;
- control-flow-aware disassembly (validated against all 552 supplied MES
  files);
- deterministic VM core for assignment, increment/decrement, J0-J3, L0-L5,
  block calls, T0/T1 text-slot tables, and external MES local-context reset;
- `UK2.CFG` parsing and UK2 virtual extension mapping (`.mes1` -> `.MES`, etc.);
- DLB Ver1.00 parsing and resource lookup behind directory files;
- PDT34 12-bit palette, rectangle, and four-plane RLE decoding at 640x400;
- MAP row-major tile-grid, 17-byte entity, and 10-byte trigger parsing with
  bounds-checked dimensions and tail counts;
- `.MMM` 13-channel directory parsing and `.MMD` 18-track delta-record
  expansion with bounds-checked channel slices;
- virtual `.mmm1` music-name resolution to the on-disk `.MMM` resource and
  confirmed M0 load, M1 volume, M2 stop, and M4 status-call VM behavior.

PC-98 rendering/input/audio handlers are exposed through `Uk2Host`; handlers
whose semantics have not yet been proven from `UK2.EXE` are not replaced by
placeholder behavior. See `UK2_REVERSE_ENGINEERING.md` for the recovered
encoding and handler table.

Useful binaries:

```text
uk2_disasm <file.MES>
uk2_verify <game-directory>
uk2_verify <game-directory> --assets
uk2_verify <game-directory> --boot-menu
uk2_verify <game-directory> --boot-path=3
uk2_verify <game-directory> --stats
uk2_player <game-directory>
```

`--assets` also decodes every PDT, parses every MAP and MMD/MMM resource, and
resolves and decodes literal PDTs referenced by UE image commands.
`--boot-menu` executes `START.MES` through the VM until the first `W8` menu,
checking each window definition and opening referenced image/music resources.
`--boot-path=3` selects the new-game option and traces the live VM path into
the first gameplay scene. `--boot-inputs=5` schedules synthetic button presses
to move through its asynchronous input loops and input-wait commands.
`--stats` reports reachable service-opcode frequency across the supplied game.
The desktop player currently presents W5 backgrounds through wgpu, renders F0
MAP tile grids from the referenced chip PDT and composites UE PDT sprite rectangles,
accepts keyboard and mouse input,
renders U2/W6 text objects in W1-defined windows and W8 choice menus using a host Japanese font when
available, and routes W6 fixed-string dialogue through the same text layer.
W7 pauses the VM until confirm/cancel input, as required by the game's
dialogue scripts; U0 controls mouse cursor visibility; W8 supports keyboard
navigation and mouse selection; WA removes the matching text object.
U1 reads `COLOR.TBL` and interpolates the displayed frame to a selected palette
bank. MAP and UE composition preserve the packed 4bpp framebuffer indices so
the palette transition recolors pixels by their original PC-98 palette index.
FA updates loaded MAP entities by ID, FB changes map cells, FC/FD change trigger
flags, F5 binds a MAP to its object ID, and FJ scrolls the clamped viewport.
The host polls button edges between VM instructions, including input loops;
FK advances the map frame; U3 displays named scene images, and FI restores the
scene background region before compositing a named PDT. MAP actors, collisions, trigger
effects, and original transition timing are not yet implemented.
The M0 resource command now keeps the loaded music score while the VM
continues; MMD/MMM sound synthesis is not yet implemented. Other high-frequency
window, effect, and device services still
need their `UK2.EXE` behavior ported before the game can be considered fully
playable.
