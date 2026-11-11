## 1. Transpilation Spike & C Source Generation

- [x] 1.1 Create a disposable Docker generator script (`scripts/regenerate_libghostty_vt_c.sh`) pinning Zig 0.16.0 (+ WABT `wasm2c` for fallback) against the patched `vendor/libghostty-vt` source with `simd = false` and `kitty_graphics = false`
- [x] 1.2 Test Tier 1 (`zig build-lib -ofmt=c` for `x86_64-linux-gnu` and `aarch64-linux-gnu` + `zig.h`); if Zig 0.16 C codegen fails or miscompiles, switch generator output to Tier 2 (`wasm32-freestanding` → `wasm2c --num-outputs=8` + minimal `wasm-rt` C99 runtime)
- [x] 1.3 Emit generated `.c`/`.h` files and `provenance.json` (source commit + active patch SHA-256 digests) into `vendor/libghostty-vt-c/`, ensuring zero `.a`/`.so`/`.wasm` binary files are kept

## 2. Cargo `cc` Build & `ghostty-vt` Crate Wiring

- [x] 2.1 Add `cc = "1"` under `[build-dependencies]` in `crates/ghostty-vt/Cargo.toml` and rewrite `crates/ghostty-vt/build.rs` to compile `vendor/libghostty-vt-c/` with `-std=c99 -O3 -fno-strict-aliasing -fwrapv` instead of invoking `zig`
- [x] 2.2 Remove Kitty-graphics-only FFI modules (`native_image_sources.rs`, `native_source.rs`, `native_source_tests.rs`, `png_forward_tests.rs`), stub Kitty graphics methods on `Terminal` as safe no-ops, and disable `crate::kitty_graphics::is_enabled()`
- [x] 2.3 If Tier 2 (`wasm2c`) was chosen in 1.2, implement the linear-memory marshaling layer and `write_pty`/side-effect callback bridge inside `crates/ghostty-vt/src/lib.rs` (no-op if Tier 1 native C ABI succeeded)

## 3. Repository Cleanup & Linux Verification

- [x] 3.1 Remove Zig build dependencies (`zig_0_16`, `build.zig.zon.nix`) from `nix/package.nix`, `flake.nix`, `Justfile`, and update vendor maintenance checks to validate `vendor/libghostty-vt-c/provenance.json`
- [x] 3.2 Verify `cargo check`, `cargo clippy --all-targets`, and `cargo build --release` succeed without `zig` in `PATH` and offline
- [x] 3.3 Run `src/pane/terminal/migration_tests.rs`, `src/client/shell/tests/input_conformance.rs`, `ui-hot-path-architecture-test`, and Linux `cargo nextest run` (gating/removing Kitty-graphics-specific tests)
