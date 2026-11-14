## Context

`herdr` isolates its entire terminal emulation dependency inside the workspace crate `crates/ghostty-vt` (extracted in commit `fff6c820`). Currently, `crates/ghostty-vt/build.rs` shells out to `zig build -Demit-lib-vt` (requiring Zig 0.16.0 and external `.zon` dependency tarballs) and links `libghostty-vt.a` via 465 KB of `bindgen` FFI (`bindings.rs`).

Outside `crates/ghostty-vt`, raw `ffi::*` items appear in only one production file (`src/pane/input.rs`) and one test file (`src/client/shell/tests/input_conformance.rs`), strictly as integer key/action/mouse enum constants — zero raw pointers cross the crate boundary. Furthermore:
- `herdr` previously maintained a complete pure-Rust key and mouse encoder (`src/input/encode.rs` and `src/input/mouse.rs` at commit `a124eed7~1`) before routing encoding through `libghostty`.
- `src/pane/kitty_keyboard.rs` (`KittyKeyboardTracker`), `src/pane/osc.rs`, `src/pane/cursor.rs`, and `src/pane/xtgettcap.rs` already track keyboard protocol stacks, OSC colors/CWD, cursor shapes, and XTGETTCAP in Rust.
- `src/pane/terminal/migration_tests.rs` provides a deterministic semantic oracle (`Observation` + `Effects`) for validating terminal backend parity.

For this Linux-only fork running text CLI/TUI coding agents, security and distribution rules forbid both Zig and prebuilt binary `.a`/`.wasm` blobs, requiring a build strictly from source with standard Rust (+ C/C++ if needed) toolchains.

## Goals / Non-Goals

**Goals:**
- Replace `crates/ghostty-vt` internals with 100% safe, auditable pure Rust so `cargo build --release` and `cargo test` work on Linux with zero Zig or C/C++ build steps.
- Preserve the existing Rust type/method facade of `crates/ghostty-vt` to minimize churn in `src/` and keep upstream rebases/merges manageable.
- Maintain multiplicative hot-path performance rules from `AGENTS.md` (O(1) scalar mode queries, retained row-level `Dirty::Partial` rendering, no per-cell heap allocations on hot paths).
- Pass `src/pane/terminal/migration_tests.rs` and agent screen detection (`herdr agent read --source detection`) for text TUI coding agents.

**Non-Goals:**
- Rendering or forwarding Kitty Graphics Protocol images (explicitly disabled/no-op for text TUI agents).
- Background Zstd/LZ4 scrollback page compression (`compress_incremental` returns `TerminalCompressionResult::Unsupported`, which `src/pane.rs` already handles natively).
- Windows ConPTY or macOS platform builds in this fork branch (Linux is the target runtime).

## Decisions

### 1. Core VT Engine: `alacritty_terminal` + `vte` behind the `ghostty-vt` facade
- **Choice**: Embed `alacritty_terminal::term::Term<HerdrEventProxy>` + `alacritty_terminal::vte::ansi::Processor` inside `ghostty_vt::Terminal`.
- **Rationale**: `alacritty_terminal` (Apache-2.0, matching `herdr`) is pure Rust, battle-tested on Linux TUI workloads, supports primary/alternate grids, scrollback reflow on resize, 256-color + RGB styles (including undercurl/dotted/dashed underlines), OSC 8 hyperlinks, OSC 52 clipboard, synchronized output (`?2026`), Kitty keyboard mode stack, and fine-grained line damage (`TermDamage`).
- **Alternatives considered**:
  - `wezterm-term`: Much heavier dependency tree and slower compile times.
  - `vt100` crate: Previously removed by `herdr` (`b2e6929a`) due to missing modern terminal modes, reflow fidelity, and damage granularity.
  - `libvterm` (C99 via `cc`): Introduces unsafe C parsing of untrusted PTY output while still lacking OSC 8, Kitty keyboard stack, and reflow features `alacritty_terminal` already provides.

### 2. Pre-stream tracker for `libghostty`-specific scalar modes & custom CSI/OSC side effects
- **Choice**: Run a lightweight streaming byte observer alongside `vte::ansi::Processor::advance` (sharing patterns from `src/pane/kitty_keyboard.rs` and `src/pane/osc.rs`) to capture scalar state not natively exposed by `alacritty_terminal`:
  - `modifyOtherKeys` level (`CSI > 4 ; level m`, `CSI > 4 n`, reset on `RIS` `\x1bc`) for O(1) `Terminal::modify_other_keys_enabled()` (replaces patch `0002`).
  - OSC 7 working directory notifications (`take_pwd_changes()`), OSC 52 writes (`take_clipboard_writes()`), bell counts (`take_bell_count()`), and PTY query replies (`Event::PtyWrite` forwarded to `write_pty_callback`).
- **Rationale**: Avoids forking `alacritty_terminal` while guaranteeing exact O(1) scalar accessors required by `herdr`'s hot-path architecture tests.

### 3. `RenderState`, `RowIterator`, and `RowCells` snapshot buffer
- **Choice**: Maintain a reusable row-snapshot vector inside `RenderState` updated during `RenderState::update(&Terminal)`. Consult `Term::damage()` (`TermDamage::Full` vs `TermDamage::Partial`) to copy only dirty viewport rows into `RenderState`'s pre-allocated row buffers, then reset `Term::reset_damage()`.
- **Rationale**: `herdr`'s renderer iterates rows and cells via `populate_row_iterator` / `populate_cells` (`grapheme_text_into(&mut bytes, &mut text)`) without holding the terminal mutex across frame serialization if needed, and expects zero per-cell allocations. Pre-allocated row/cell buffers preserve exact borrow lifetimes and zero-allocation hot paths.

### 4. Pure-Rust `KeyEncoder` and `MouseEncoder`
- **Choice**: Re-introduce a pure-Rust `ffi` constants module (`crates/ghostty-vt/src/ffi.rs` re-exported as `pub mod bindings; pub use bindings as ffi;`) defining the exact `GhosttyKey`, `GhosttyKeyAction`, `GhosttyMouse*` integer constants used by `src/pane/input.rs`, and implement `KeyEncoder` + `MouseEncoder` in pure Rust by adapting `herdr`'s own pre-`#5017` encoder (`a124eed7~1:src/input/encode.rs` and `mouse.rs`).
- **Rationale**: Eliminates `bindgen` and C ABI structs while keeping `src/pane/input.rs` untouched and preserving key/mouse protocol behavior.

### 5. Local patch equivalents & Kitty graphics opt-out
- **Patch `0005` (bounded wrapped word selection)**: Implement directly over `alacritty_terminal::grid::Grid` using `cell.flags.contains(Flags::WRAPLINE)` and `Flags::WIDE_CHAR_SPACER` with the same shared cell-inspection budget.
- **Patch `0006` (`clear_screen` preserving cursor line)**: Extract the current logical soft-wrapped cursor row(s) on the primary screen, invoke history/screen clear on `Term`, and restore the preserved logical line and cursor column.
- **Kitty graphics**: Default `crate::kitty_graphics::is_enabled()` (or `ghostty-vt` capability) to disabled/no-op; `enable_kitty_graphics()` returns `Ok(())`, `kitty_graphics_may_have_placements()` returns `Ok(false)`.

## Risks / Trade-offs

- **[Subtle VT edge-case divergence from `libghostty-vt`]** → Mitigation: Use `src/pane/terminal/migration_tests.rs` as the mandatory conformance gate; any discrepancy in wrap flags, `ED 3` scrollback clear, DECRQM replies, or ANSI formatting (`read_ansi_viewport`/`read_ansi_screen`) is caught deterministically.
- **[`input_conformance.rs` previously used `libghostty` as external oracle]** → Mitigation: Keep protocol-vector unit tests against known xterm / Kitty CSI-u expected byte sequences so the pure-Rust `KeyEncoder` does not test itself tautologically.
- **[Scrollback memory limit units (`max_scrollback` bytes vs lines)]** → Mitigation: `herdr` passes `scrollback_limit_bytes` to `Terminal::new(cols, rows, max_scrollback)`. Convert bytes to an approximate line cap (`max_scrollback / (cols as usize * cell_byte_estimate).max(1)`) when configuring `alacritty_terminal::term::Config::scrolling_history`.
- **[Upstream merge conflicts when syncing fork]** → Mitigation: Keep edits concentrated inside `crates/ghostty-vt/` and build/CI files (`Cargo.toml`, `Justfile`, `nix/package.nix`), leaving `src/` nearly untouched.
