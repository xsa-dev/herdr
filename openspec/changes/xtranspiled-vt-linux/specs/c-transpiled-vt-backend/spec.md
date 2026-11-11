## ADDED Requirements

### Requirement: Standard Rust + C compiler build without Zig or binary blobs
The workspace SHALL compile `crates/ghostty-vt` and `herdr` on Linux from checked-in Rust and C99 source files using only `cargo`, `rustc`, and a standard C compiler (`cc`/`gcc`/`clang`), without invoking `zig`, fetching `.zon` packages, or linking prebuilt `.a`/`.so`/`.wasm` binary artifacts.

#### Scenario: Offline Linux build without Zig installed
- **WHEN** `cargo check`, `cargo test`, or `cargo build --release` runs on Linux with `zig` absent from `PATH` and network disabled
- **THEN** `crates/ghostty-vt/build.rs` compiles the C99 sources under `vendor/libghostty-vt-c/` via the `cc` crate and links `herdr` cleanly

### Requirement: Preservation of patched libghostty-vt text-terminal semantics
The C-transpiled `libghostty-vt` unit SHALL include `herdr`'s active text-terminal patches (`0002` `modifyOtherKeys` scalar query, `0005` bounded wrapped word selection, `0006` clear screen preserving cursor line, and `0008` cursor hyperlink page replacement safety) and pass `herdr`'s terminal semantic and input conformance suites.

#### Scenario: Patched C ABI operations execute identically
- **WHEN** `herdr` exercises `modify_other_keys_enabled`, `clear_screen`, `viewport_link_target`, `RenderState` dirty row iteration, and `KeyEncoder` / `MouseEncoder`
- **THEN** results match `libghostty-vt`'s behavior across `src/pane/terminal/migration_tests.rs` and `src/client/shell/tests/input_conformance.rs`

### Requirement: Self-contained C translation unit without Kitty graphics or C++ SIMD deps
The generated C sources SHALL be produced with `kitty_graphics = false` and `simd = false` so no external `wuffs`, `simdutf`, `highway`, or `libstdc++`/`libc++` dependencies are required at build or link time, while Kitty graphics queries in `crates/ghostty-vt` behave as safe no-ops.

#### Scenario: Kitty graphics disabled at runtime
- **WHEN** `herdr` initializes a pane terminal or queries `kitty_graphics_may_have_placements` / `kitty_image_placements`
- **THEN** initialization succeeds without requiring Kitty graphics C symbols and placement queries return `Ok(false)` / empty `Vec`

### Requirement: Reproducible source regeneration script
The repository SHALL include a deterministic maintainer/fork script (`scripts/regenerate_libghostty_vt_c.sh`) that regenerates `vendor/libghostty-vt-c/` in an isolated Linux container from pinned `libghostty-vt` source commit + local patches and records provenance metadata (source commit, patch digest, generator version).

#### Scenario: Verifying provenance metadata
- **WHEN** repository maintenance checks inspect `vendor/libghostty-vt-c/provenance.json`
- **THEN** the recorded `source_commit` and patch hashes match the documented `libghostty-vt` base and active text patches
