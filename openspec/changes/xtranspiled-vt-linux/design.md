## Context

`herdr` embeds `libghostty-vt` through `crates/ghostty-vt` (`fff6c820`). Currently `crates/ghostty-vt/build.rs` executes `zig build -Demit-lib-vt`, requiring Zig 0.16.0 and external network tarballs (`uucode`, `translate_c`, `wuffs`, `simdutf`, `highway`).

Inspection of `vendor/libghostty-vt/src/build/GhosttyZig.zig` (`initVt`, lines 140–158) shows a critical simplification when targeting Linux text CLI/TUI agents:
- Disabling Kitty graphics (`kitty_graphics = false`) drops `wuffs` and `translate_c` completely.
- Setting `simd = false` drops `simdutf`, `highway`, and C++ compiler/runtime requirements completely.
- What remains is a single self-contained Zig module (`lib_vt.zig` + `unicode_tables` + `uucode`) plus text-focused local patches (`0002`, `0005`, `0006`, `0008`; patches `0004` Wuffs and `0007` Kitty PNG retention become unnecessary when Kitty graphics is off).

To satisfy a strict "no binary `.a`/`.wasm` blobs, standard Rust + C/C++ toolchain from source" requirement in this Linux fork while preserving `libghostty-vt`'s exact engine, we pre-transpile that self-contained module into plain C99 source files compiled by Cargo's `cc` crate.

## Goals / Non-Goals

**Goals:**
- Build and test `herdr` on Linux (`x86_64` and `aarch64`) using only `rustc`, `cargo`, and a standard C99 compiler (`gcc`/`clang`) with zero `.a`/`.so`/`.wasm` blobs in git.
- Retain 100% of `libghostty-vt`'s VT state machine, page-list scrollback (including incremental compression if compiled in), key/mouse encoding, and active text patches (`0002`, `0005`, `0006`, `0008`).
- Provide a repeatable Docker-based generator script so the host machine never needs Zig installed.

**Non-Goals:**
- Kitty Graphics Protocol image decoding/forwarding (`kitty_graphics = false`; `crate::kitty_graphics::is_enabled()` disabled).
- C++ SIMD acceleration (`simdutf`/`highway`) — scalar fallbacks inside `libghostty-vt` are already tested in upstream (`LIBGHOSTTY_VT_SIMD=false`).
- Windows or macOS target support in this Linux fork branch.

## Decisions

### 1. Two-tier C transpilation strategy (Spike `-ofmt=c` first, fallback to `wasm32` → `wasm2c`)
- **Tier 1 (Preferred if Zig 0.16 CBE succeeds): Direct Zig C backend (`-ofmt=c`)**
  - Emit C directly from the `vt_c` root module with `simd = false`, `kitty_graphics = false`, for `x86_64-linux-gnu` and `aarch64-linux-gnu` (selected via `#[cfg(target_arch)]` / `TARGET` in `build.rs`), bundled alongside Zig's runtime header `zig.h`.
  - **Why try first**: Exports the **exact native C ABI** (`ghostty_terminal_new`, host pointers, direct C callbacks). `crates/ghostty-vt/src/lib.rs` (5,000+ lines) and `bindings.rs` require **zero pointer-marshaling changes** — only stubbing removed Kitty graphics symbols!
- **Tier 2 (Guaranteed fallback if Zig 0.16 `-ofmt=c` hits a codegen bug): `wasm32-freestanding` → `wasm2c` (WABT)**
  - `GhosttyLibVt.zig` (`initWasm`) already builds `ghostty-vt.wasm` via Zig's mature LLVM backend. Running `wasm2c` produces portable ANSI C99 (`ghostty_vt_wasm.c` + `ghostty_vt_wasm.h` + ~300-line `wasm-rt-impl.c`) that compiles identically on **all** 64-bit Linux architectures from a single `.c` source.
  - **Trade-off**: `wasm2c` functions take a module instance pointer and `u32` offsets into the module's linear memory instead of host pointers, requiring an FFI marshaling layer inside `crates/ghostty-vt` (allocating scratch buffers inside module memory via `ghostty_wasm_alloc_u8_array` and dispatching `write_pty` via imported host functions).
- **Execution rule**: Run a 15-minute Docker spike on Tier 1 first; commit Tier 1 if all `ghostty-vt` + `herdr` tests compile and pass under `gcc`/`clang`, otherwise pivot immediately to Tier 2.

### 2. Compile via Cargo `cc` crate in `crates/ghostty-vt/build.rs`
- **Choice**: Replace `std::process::Command::new("zig")` in `crates/ghostty-vt/build.rs` with `cc::Build::new()` compiling `vendor/libghostty-vt-c/*.c` with `-std=c99 -O3 -fno-strict-aliasing -Wno-unused-*` (matching Zig C / `wasm2c` expectations).
- **Rationale**: `cc` is the standard Rust ecosystem build-dependency, honors `CC`/`CFLAGS` in Linux distro builders and Nix `stdenv`, and caches compiled `.o`/`.a` inside Cargo's `OUT_DIR`.

### 3. Disable Kitty graphics cleanly across `ghostty-vt` and `herdr`
- **Choice**: Turn off Kitty graphics in the generated `libghostty-vt` build options (`kitty_graphics = false`), stub out Kitty graphics FFI calls in `crates/ghostty-vt/src/lib.rs` (`enable_kitty_graphics() -> Ok(())`, `kitty_graphics_may_have_placements() -> Ok(false)`), delete `native_image_sources.rs` / `native_source.rs` / `png_forward_tests.rs`, and make `crate::kitty_graphics::is_enabled()` return `false`.
- **Rationale**: Eliminates `wuffs` C translation and Linux CoW file-backing callbacks (`0007`) while keeping `herdr`'s text TUI paths intact.

## Risks / Trade-offs

- **[Large generated `.c` file size & compiler time]** → Mitigation: With `simd=false` and `kitty_graphics=false`, dead-code elimination shrinks emitted C substantially; split into chunks (`wasm2c --num-outputs=N`) if parallel `cc` compilation speed is needed, and disable debug info (`debug(false)`) on the generated C unit during `cargo test` so `cc` finishes in seconds.
- **[Strict strict-aliasing / UB warnings in GCC/Clang on generated C]** → Mitigation: Always pass `-fno-strict-aliasing -fwrapv` in `cc::Build`, which both `zig.h` and `wasm2c` assume for defined two's-complement / type-punning semantics.
- **[Security audit pushback on machine-generated C]** → Mitigation: Keep `scripts/regenerate_libghostty_vt_c.sh` + `provenance.json` in the repo so auditors can verify deterministic reproduction from upstream source + patches, while maintaining the sibling `pure-rust-vt-lunux` branch for environments requiring hand-written source only.
