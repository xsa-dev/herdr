## 1. Crate & Dependency Setup

- [x] 1.1 Remove `crates/ghostty-vt/build.rs` and add `alacritty_terminal`, `unicode-width`, and `unicode-segmentation` to `crates/ghostty-vt/Cargo.toml`
- [x] 1.2 Replace generated `crates/ghostty-vt/src/bindings.rs` with a pure-Rust constants module (`ffi` / `bindings`) preserving `GhosttyKey`, `GhosttyKeyAction`, `GhosttyMouseAction`, `GhosttyMouseButton`, `GhosttyMouseFormat`, and modifier bitmasks used by `src/pane/input.rs`
- [x] 1.3 Remove native Kitty source FFI modules (`native_image_sources.rs`, `native_source.rs`, `native_source_tests.rs`, `png_forward_tests.rs`) while keeping pure-Rust `pane_graphics_files.rs`

## 2. Core Terminal Grid, Modes, and Text/ANSI Readers

- [x] 2.1 Implement `Terminal` backed by `alacritty_terminal::term::Term` + `vte::ansi::Processor`, mapping scrollback byte limits, `resize`, `scroll_viewport_*`, `active_screen`, `scrollbar`, and palette/color-scheme state
- [x] 2.2 Implement streaming side-effect and mode observer (`EventProxy` + byte pre-tracker) for `write_pty_callback` replies, `modifyOtherKeys` mode 2 (`CSI > 4 ; 2 m`), Kitty keyboard flag stack, OSC 7 CWD changes, OSC 52 clipboard writes, and bell counts
- [x] 2.3 Implement cell/row text and ANSI formatters (`screen_cell`, `screen_text_rows`, `for_each_screen_row_cell`, `screen_text_rows_range`, `read_text_viewport`, `read_ansi_viewport`, `read_text_screen`, `read_ansi_screen`, `track_row`) without per-cell hot-path heap allocations
- [x] 2.4 Implement local patch parity helpers: `clear_screen` preserving the cursor's soft-wrapped primary-screen line, and bounded wrap-aware `viewport_link_target` / `viewport_link_regions` (OSC 8 + URL word scan skipping wide spacers)
- [x] 2.5 Implement no-op stubs for Kitty graphics (`enable_kitty_graphics`, `kitty_graphics_may_have_placements -> false`, `kitty_image_placements -> empty`) and compression (`compress_incremental -> Unsupported`), and disable Kitty graphics by default in `src/kitty_graphics.rs` if needed

## 3. Incremental Damage & RenderState Iterators

- [x] 3.1 Implement `RenderState` with reusable row/cell snapshot buffers driven by `Term::damage()` (`Dirty::Clean`, `Dirty::Partial`, `Dirty::Full`), cursor visual state (`RenderCursor`), and `RenderColors`
- [x] 3.2 Implement `RowIterator` (`RowIter::next`, `next_dirty`, `dirty`, `wrap_state`, `selection`) and `RowCells` (`select`, `is_default_blank`, `basic_data`, `wide`, `has_hyperlink`, `style`, `content_bg_color`, `fg_color`, `bg_color`, `grapheme_text_into`)

## 4. Pure-Rust Key and Mouse Encoding

- [x] 4.1 Implement `KeyEvent` and `KeyEncoder` in pure Rust (adapting `a124eed7~1:src/input/encode.rs`) supporting legacy xterm, `modifyOtherKeys` mode 2, and Kitty CSI-u flag stack (`press`, `repeat`, `release`, text, shifted/unshifted codepoints)
- [x] 4.2 Implement `MouseEvent` and `MouseEncoder` in pure Rust supporting X10, UTF-8, SGR (`1006`), and SGR-pixel (`1016`) mouse reporting plus button/motion tracking
- [x] 4.3 Implement Unicode width helpers (`unicode_codepoint_width`, `unicode_grapheme_width`) and `encode_focus` in pure Rust

## 5. Vendor Cleanup, Build Wiring, and Conformance Verification

- [x] 5.1 Remove `vendor/libghostty-vt/`, `vendor/libghostty-vt.vendor.json`, `vendor/libghostty-vt.patches.md`, `vendor/patches/libghostty-vt/`, and Zig references in `nix/package.nix`, `flake.nix`, `Justfile`, and maintenance tests
- [x] 5.2 Run `cargo check` and `cargo clippy --all-targets` without Zig installed and resolve any remaining compiler or lint issues
- [x] 5.3 Verify terminal behavior against `src/pane/terminal/migration_tests.rs`, input encoding vector tests, `ui-hot-path-architecture-test`, and Linux `cargo nextest run` (gating/removing obsolete Kitty-graphics-only tests)
