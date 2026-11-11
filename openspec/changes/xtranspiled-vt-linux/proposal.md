## Why

`herdr` currently shells out to `zig build` (requiring Zig 0.16.0 and external `.zon` network fetches) to build `libghostty-vt`, preventing builds in strict Linux packaging and security environments that allow only standard Rust + C/C++ toolchains with zero prebuilt `.a`/`.so`/`.wasm` binary blobs. Pre-transpiling the patched `libghostty-vt` terminal engine (with Kitty graphics and C++ SIMD disabled) into plain C99 source files compiled via Cargo's `cc` crate preserves exact `libghostty-vt` VT/scrollback/input semantics while requiring only `rustc`, `cargo`, and a standard C compiler (`gcc`/`clang`).

## What Changes

- Pre-transpile the patched `vendor/libghostty-vt` (`simd = false`, `kitty_graphics = false`) into checked-in C99 source + header files under `vendor/libghostty-vt-c/` using a reproducible one-off Docker generator script (evaluating Zig's direct `-ofmt=c` backend first for zero-overhead native C ABI, with `wasm32-freestanding` → `wasm2c` as deterministic architecture-independent fallback).
- Rewrite `crates/ghostty-vt/build.rs` to compile the C99 sources directly with the Rust `cc` crate (`-O3`), completely eliminating `zig` from `cargo check`, `cargo test`, and `cargo build`.
- If the `wasm2c` fallback path is selected over `-ofmt=c`, adapt `crates/ghostty-vt/src/lib.rs` FFI calls to marshal slices/callbacks through the `wasm2c` module's linear memory instance while keeping `ghostty-vt`'s public Rust API unchanged.
- **BREAKING** (fork-scoped): Disable Kitty Graphics Protocol (`kitty_graphics = false` / no-op placements) and external C++ SIMD (`simdutf`, `highway`, `wuffs` excluded from the C unit), targeting Linux text CLI/TUI agent workflows.
- Remove the runtime build dependency on `zig_0_16` and `zon2nix` from `nix/package.nix`, `flake.nix`, and Linux build recipes.

## Capabilities

### New Capabilities
- `c-transpiled-vt-backend`: Zig-free, blob-free C99 source distribution of `libghostty-vt` compiled via Cargo's `cc` crate on Linux, preserving `libghostty-vt` terminal, scrollback, link-selection, and input-encoding behavior for text TUI agents.

### Modified Capabilities
<!-- None -->

## Impact

- **Crates & Build**: `crates/ghostty-vt/Cargo.toml` (adds `[build-dependencies] cc = "1"`), `crates/ghostty-vt/build.rs` (replaces `Command::new("zig")` with `cc::Build`), and `crates/ghostty-vt/src/lib.rs` (untouched ABI if `-ofmt=c` succeeds, or linear-memory adapter if `wasm2c` is used; Kitty graphics stubbed).
- **Vendored Sources & Tooling**: Adds `vendor/libghostty-vt-c/` (`.c` + `.h` sources) and an offline-reproducible Docker regeneration script `scripts/regenerate_libghostty_vt_c.sh`; removes or archives the Zig build tree requirement for downstream consumers.
- **Packaging**: Simplifies `nix/package.nix` and `flake.nix` on Linux to standard `rustPlatform.buildRustPackage` + `stdenv.cc` without `zig_0_16` or `build.zig.zon.nix`.
