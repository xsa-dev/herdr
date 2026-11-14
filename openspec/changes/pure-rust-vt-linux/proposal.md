## Why

Building `herdr` currently requires Zig 0.16.0 and external Zig dependency fetches to compile the vendored `libghostty-vt` static library, which violates strict from-source security/distribution policies that allow only standard Rust and C/C++ toolchains with zero prebuilt binary blobs. Replacing the internals of `crates/ghostty-vt` with an auditable pure-Rust terminal backend eliminates the Zig toolchain and native C/Zig vendor tree completely for our Linux text-agent fork.

## What Changes

- Replace the Zig/FFI implementation inside `crates/ghostty-vt` with a pure-Rust terminal emulation backend built on `alacritty_terminal` (plus `vte`, `unicode-width`, and `unicode-segmentation`) while preserving the safe Rust public API consumed by `src/`.
- Port/restore pure-Rust keyboard and mouse protocol encoding (`KeyEncoder`, `MouseEncoder`, Kitty CSI-u, `modifyOtherKeys`, SGR/SGR-pixel mouse) inside `crates/ghostty-vt`, drawing from `herdr`'s prior pure-Rust encoder (`a124eed7~1`) and `KittyKeyboardTracker`.
- Implement pure-Rust equivalents for the text-focused local `libghostty-vt` patch behaviors: `modifyOtherKeys` mode 2 query, bounded wrapped-line word selection for Ctrl+click link activation, and cursor-line-preserving screen clear.
- **BREAKING** (fork-scoped): Disable Kitty Graphics Protocol image rendering/forwarding (`kitty_graphics` becomes a safe no-op returning no placements) and background page compression (`TerminalCompressionResult::Unsupported`), targeting text-based CLI/TUI coding agents on Linux.
- Remove `crates/ghostty-vt/build.rs`, `vendor/libghostty-vt*`, `vendor/patches/libghostty-vt/`, and Zig-specific Linux/Nix build wiring so `cargo check`, `cargo test`, and `cargo build --release` succeed offline with `rustc` and `cargo` alone.

## Capabilities

### New Capabilities
- `pure-rust-vt-backend`: Pure-Rust implementation of the `ghostty-vt` workspace crate contract (VT parsing, primary/alternate screen grids, scrollback, row-level damage/render iteration, OSC/link queries, key/mouse encoding, and Kitty graphics no-op) buildable on Linux without Zig or prebuilt archives.

### Modified Capabilities
<!-- None -->

## Impact

- **Crates & Build**: `crates/ghostty-vt/Cargo.toml`, removal of `crates/ghostty-vt/build.rs` and `crates/ghostty-vt/src/bindings.rs` (replaced by pure-Rust key/mouse/mode constants where referenced by `src/pane/input.rs`).
- **Vendored Code & Scripts**: Removal of `vendor/libghostty-vt/`, `vendor/libghostty-vt.vendor.json`, `vendor/libghostty-vt.patches.md`, `vendor/patches/libghostty-vt/`, and Zig checks in `Justfile` / `scripts/test_vendor_libghostty_vt.py` / `nix/package.nix`.
- **Dependencies**: Adds `alacritty_terminal` (and `vte` if needed directly) to `Cargo.toml` / `Cargo.lock` (Apache-2.0, pure Rust).
- **Tests**: Preserves `src/pane/terminal/migration_tests.rs` semantic conformance; updates or gates Kitty-graphics-specific tests and `libghostty` vendor maintenance tests.
