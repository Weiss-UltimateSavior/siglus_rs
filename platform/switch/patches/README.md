# Patched crates for the Switch build

The Switch build uses the ordinary crates.io dependency graph: the target
(`rust/aarch64-switch.json`, `env = "newlib"`) builds `std` with
`-Zbuild-std`, so no dependency needs a `no_std` port. This directory holds
only crates that do not compile with the pinned nightly
(`nightly-2026-01-20`) and have no fixed release yet.

| Crate | Patch | Why |
|---|---|---|
| `unwinding` 0.2.10 | `unwinding-0.2.10-catch-unwind-i32.patch` | `core::intrinsics::catch_unwind` now returns `i32` rather than `bool`; upstream 0.2.10 (the newest release, and `trunk` as of this writing) still treats it as `bool`. |

## Rules

- A vendored crate is its crates.io release plus exactly the listed `.patch`.
  `./verify.sh` downloads the release, checks its crates.io checksum,
  applies the patch and diffs the result against the vendored copy.
  `./verify.sh --upstream` also reports the newest release, so the copy can
  be deleted once upstream ships the fix.
- Patches are applied only to the Switch build, never workspace-wide. Nothing in
  the current NRO depends on `unwinding` (it is needed by libnx Rust bindings
  such as `nx`), so the Makefile passes it only on request:

  ```sh
  make -C platform/switch/runtime UNWINDING_PATCH=1
  ```

  which adds `--config 'patch.crates-io.unwinding.path="platform/switch/patches/unwinding-0.2.10"'`
  to the Rust build.
- These directories are not workspace members or path dependencies, so
  `cargo fmt` never rewrites them. Keep it that way: edit the `.patch`, then
  regenerate the vendored copy from the release.

## Updating or adding a patch

```sh
curl -sSfL -o x.crate https://static.crates.io/crates/<name>/<name>-<ver>.crate
tar xzf x.crate
cd <name>-<ver> && patch -p1 < ../<patch> && cd ..
rm -rf platform/switch/patches/<name>-<ver>
mv <name>-<ver> platform/switch/patches/
```

Then add the crate to `CRATES` in `verify.sh` (name, version, the `.crate`
SHA-256 from the crates.io index, patch file) and run `./verify.sh`.
