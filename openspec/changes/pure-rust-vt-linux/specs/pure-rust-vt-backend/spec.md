## ADDED Requirements

### Requirement: Zig-free and blob-free Linux source build
The workspace SHALL compile `crates/ghostty-vt` and the `herdr` binary on Linux strictly from auditable Rust source code using standard `cargo` and `rustc` without invoking `zig`, fetching external Zig packages, or linking prebuilt `.a`/`.so`/`.wasm` binary archives.

#### Scenario: Building and testing without Zig installed
- **WHEN** `cargo check`, `cargo test`, or `cargo build --release` is executed on a Linux environment where `zig` is absent from `PATH` and network access is disabled (with vendored/locked Cargo crates)
- **THEN** compilation and linking succeed using pure-Rust dependencies only

### Requirement: Drop-in Terminal screen, scrollback, and damage API
`crates/ghostty-vt` SHALL preserve the safe Rust `Terminal`, `RenderState`, `RowIterator`, and `RowCells` API contract consumed by `herdr`, providing primary and alternate screen buffers, scrollback history with resize reflow, 256-color and RGB styling, OSC 7/8/52 side effects, and row-level dirty tracking (`Dirty::Clean`, `Dirty::Partial`, `Dirty::Full`).

#### Scenario: Incremental render damage updates only modified rows
- **WHEN** PTY bytes modify a subset of visible rows on an already-cleaned `RenderState`
- **THEN** `RenderState::update` reports `Dirty::Partial` and `RowIter::next_dirty` yields each modified row index without requiring a full-screen redraw

#### Scenario: Clear screen preserving visible cursor line
- **WHEN** `Terminal::clear_screen` is invoked on the primary screen while the cursor sits on a soft-wrapped logical line
- **THEN** scrollback history and other screen lines are cleared while the cursor's visible soft-wrapped line is preserved, and calls on the alternate screen leave the alternate screen untouched

#### Scenario: Bounded wrapped link selection
- **WHEN** `Terminal::viewport_link_target` or `viewport_link_regions` inspects a URL wrapped across multiple viewport rows within the cell inspection budget
- **THEN** the full wrapped URL is resolved skipping wide-character spacer cells, and an over-budget line returns `None` rather than a truncated link

### Requirement: Pure-Rust keyboard and mouse protocol encoding
`crates/ghostty-vt` SHALL encode `KeyEvent` and `MouseEvent` inputs in pure Rust according to the terminal's active modes, including legacy xterm sequences, `modifyOtherKeys` mode 2, Kitty keyboard protocol (`CSI u` flag stack), and X10/UTF-8/SGR/SGR-pixel mouse formats.

#### Scenario: Querying modifyOtherKeys mode 2 without formatting
- **WHEN** a child application enables `CSI > 4 ; 2 m` and `Terminal::modify_other_keys_enabled` is called
- **THEN** the terminal returns `Ok(true)` in O(1) scalar access without formatting screen or scrollback buffers, and resets to `Ok(false)` upon `CSI > 4 m`, `CSI > 4 ; 0 m`, or `RIS` (`\x1bc`)

#### Scenario: Encoding keys under Kitty keyboard protocol flags
- **WHEN** a child application pushes Kitty keyboard flags (`CSI > flags u`) and `KeyEncoder::set_from_terminal` followed by `KeyEncoder::encode` is invoked
- **THEN** `KeyEncoder` emits the matching `CSI u` sequence for press, repeat, or release events according to the active flag bits

### Requirement: Graceful no-op for disabled Kitty graphics and page compression
`crates/ghostty-vt` SHALL treat Kitty graphics and background scrollback compression as unsupported/disabled no-ops so text-based CLI/TUI agent workflows operate without errors or visual artifacts.

#### Scenario: Kitty graphics APC sequences in PTY output
- **WHEN** a child process writes Kitty graphics `APC G ... ST` sequences or `herdr` queries `kitty_graphics_may_have_placements` and `kitty_image_placements`
- **THEN** the parser ignores/consumes the APC payload without corrupting surrounding terminal text and placement queries return `Ok(false)` / empty `Vec`

#### Scenario: Background compression tick
- **WHEN** `herdr` invokes `Terminal::compress_incremental`
- **THEN** it returns `Ok(TerminalCompressionResult::Unsupported)` without mutating screen or scrollback state
