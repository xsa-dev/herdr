//! Pure-Rust terminal emulation, incremental damage snapshots, and input encoding.

// `bindings` preserves the C-style constant names consumed by `src/pane/input.rs` and tests.
#[allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]
pub mod bindings;
mod encode;
mod format;
pub mod pane_graphics_files;
mod render;
mod terminal;
mod terminfo;
mod unicode;

use std::fmt;
use std::ops::RangeInclusive;
use std::sync::Arc;

use crate::pane_graphics_files::OwnedExport;

/// Terminfo entry the terminal emulates; child processes should see it as TERM.
pub const TERM: &str = "xterm-256color";

pub use bindings as ffi;
pub use encode::{encode_focus, KeyEncoder, KeyEvent, MouseEncoder, MouseEvent};
pub use render::{CellBasicData, RenderState, RowCellIter, RowCells, RowIter, RowIterator};
pub use terminal::Terminal;
pub use unicode::{unicode_codepoint_width, unicode_grapheme_width};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error(pub(crate) ffi::GhosttyResult);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ghostty error {}", self.0)
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dirty {
    Clean,
    Partial,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalCompressionResult {
    Unsupported,
    Pending,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowSelection {
    pub start_x: u16,
    pub end_x: u16,
}

impl RowSelection {
    pub fn range(self) -> RangeInclusive<u16> {
        self.start_x..=self.end_x
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusEvent {
    Gained,
    Lost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorScheme {
    Light,
    Dark,
}

pub const MOD_SHIFT: u16 = ffi::GHOSTTY_MODS_SHIFT as u16;
pub const MOD_CTRL: u16 = ffi::GHOSTTY_MODS_CTRL as u16;
pub const MOD_ALT: u16 = ffi::GHOSTTY_MODS_ALT as u16;
pub const MOD_SUPER: u16 = ffi::GHOSTTY_MODS_SUPER as u16;

pub const KEY_ENTER: ffi::GhosttyKey = ffi::GhosttyKey_GHOSTTY_KEY_ENTER;
pub const KEY_UP: ffi::GhosttyKey = ffi::GhosttyKey_GHOSTTY_KEY_ARROW_UP;
pub const KEY_DOWN: ffi::GhosttyKey = ffi::GhosttyKey_GHOSTTY_KEY_ARROW_DOWN;
pub const KEY_LEFT: ffi::GhosttyKey = ffi::GhosttyKey_GHOSTTY_KEY_ARROW_LEFT;
pub const KEY_RIGHT: ffi::GhosttyKey = ffi::GhosttyKey_GHOSTTY_KEY_ARROW_RIGHT;
pub const KEY_A: ffi::GhosttyKey = ffi::GhosttyKey_GHOSTTY_KEY_A;

pub const MOUSE_ACTION_PRESS: ffi::GhosttyMouseAction =
    ffi::GhosttyMouseAction_GHOSTTY_MOUSE_ACTION_PRESS;
pub const MOUSE_ACTION_RELEASE: ffi::GhosttyMouseAction =
    ffi::GhosttyMouseAction_GHOSTTY_MOUSE_ACTION_RELEASE;
pub const MOUSE_ACTION_MOTION: ffi::GhosttyMouseAction =
    ffi::GhosttyMouseAction_GHOSTTY_MOUSE_ACTION_MOTION;
pub const MOUSE_BUTTON_LEFT: ffi::GhosttyMouseButton =
    ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_LEFT;
pub const MOUSE_BUTTON_RIGHT: ffi::GhosttyMouseButton =
    ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_RIGHT;
pub const MOUSE_BUTTON_MIDDLE: ffi::GhosttyMouseButton =
    ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_MIDDLE;
pub const MOUSE_BUTTON_WHEEL_UP: ffi::GhosttyMouseButton =
    ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_FOUR;
pub const MOUSE_BUTTON_WHEEL_DOWN: ffi::GhosttyMouseButton =
    ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_FIVE;
pub const MOUSE_BUTTON_WHEEL_LEFT: ffi::GhosttyMouseButton =
    ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_SIX;
pub const MOUSE_BUTTON_WHEEL_RIGHT: ffi::GhosttyMouseButton =
    ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_SEVEN;
pub const MOUSE_FORMAT_SGR: ffi::GhosttyMouseFormat =
    ffi::GhosttyMouseFormat_GHOSTTY_MOUSE_FORMAT_SGR;
pub const MOUSE_FORMAT_SGR_PIXELS: ffi::GhosttyMouseFormat =
    ffi::GhosttyMouseFormat_GHOSTTY_MOUSE_FORMAT_SGR_PIXELS;

pub const MODE_APPLICATION_CURSOR_KEYS: u16 = 1;
pub const MODE_FOCUS_EVENT: u16 = 1004;
pub const MODE_MOUSE_UTF8: u16 = 1005;
pub const MODE_MOUSE_SGR: u16 = 1006;
pub const MODE_MOUSE_ALTERNATE_SCROLL: u16 = 1007;
pub const MODE_MOUSE_SGR_PIXELS: u16 = 1016;
pub const MODE_BRACKETED_PASTE: u16 = 2004;
pub const MODE_SYNCHRONIZED_OUTPUT: u16 = 2026;
pub const MODE_GRAPHEME_CLUSTER: u16 = 2027;
pub const MODE_COLOR_SCHEME_REPORT: u16 = 2031;

pub const KITTY_UNICODE_PLACEHOLDER: u32 = 0x10EEEE;

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub enum KittyImageFormat {
    Rgb,
    Rgba,
    Png,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KittyImagePlacement {
    pub image_id: u32,
    pub placement_id: u32,
    pub z: i32,
    pub x_offset: u32,
    pub y_offset: u32,
    pub image_width: u32,
    pub image_height: u32,
    pub format: KittyImageFormat,
    pub data_len: usize,
    pub data_fingerprint: u64,
    pub data: Vec<u8>,
    pub source_file: Option<Arc<OwnedExport>>,
    pub render: KittyPlacementRenderInfo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KittyImageDescriptor {
    pub image_id: u32,
    pub placement_id: u32,
    pub image_width: u32,
    pub image_height: u32,
    pub format: KittyImageFormat,
    pub data_len: usize,
    pub data_fingerprint: u64,
    pub source_file: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KittyPlacementRenderInfo {
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub grid_cols: u32,
    pub grid_rows: u32,
    pub viewport_col: i32,
    pub viewport_row: i32,
    pub source_x: u32,
    pub source_y: u32,
    pub source_width: u32,
    pub source_height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorVisualStyle {
    Bar,
    Block,
    Underline,
    BlockHollow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveScreen {
    Primary,
    Alternate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalScrollbar {
    pub total: usize,
    pub offset: usize,
    pub len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorViewport {
    pub x: u16,
    pub y: u16,
    pub wide_tail: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderCursor {
    pub viewport: Option<CursorViewport>,
    pub visible: bool,
    pub blinking: bool,
    pub visual_style: CursorVisualStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RgbColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

const DEFAULT_PALETTE: [RgbColor; 256] = {
    let mut palette = [RgbColor { r: 0, g: 0, b: 0 }; 256];
    palette[0] = RgbColor {
        r: 0x1D,
        g: 0x1F,
        b: 0x21,
    };
    palette[1] = RgbColor {
        r: 0xCC,
        g: 0x66,
        b: 0x66,
    };
    palette[2] = RgbColor {
        r: 0xB5,
        g: 0xBD,
        b: 0x68,
    };
    palette[3] = RgbColor {
        r: 0xF0,
        g: 0xC6,
        b: 0x74,
    };
    palette[4] = RgbColor {
        r: 0x81,
        g: 0xA2,
        b: 0xBE,
    };
    palette[5] = RgbColor {
        r: 0xB2,
        g: 0x94,
        b: 0xBB,
    };
    palette[6] = RgbColor {
        r: 0x8A,
        g: 0xBE,
        b: 0xB7,
    };
    palette[7] = RgbColor {
        r: 0xC5,
        g: 0xC8,
        b: 0xC6,
    };
    palette[8] = RgbColor {
        r: 0x66,
        g: 0x66,
        b: 0x66,
    };
    palette[9] = RgbColor {
        r: 0xD5,
        g: 0x4E,
        b: 0x53,
    };
    palette[10] = RgbColor {
        r: 0xB9,
        g: 0xCA,
        b: 0x4A,
    };
    palette[11] = RgbColor {
        r: 0xE7,
        g: 0xC5,
        b: 0x47,
    };
    palette[12] = RgbColor {
        r: 0x7A,
        g: 0xA6,
        b: 0xDA,
    };
    palette[13] = RgbColor {
        r: 0xC3,
        g: 0x97,
        b: 0xD8,
    };
    palette[14] = RgbColor {
        r: 0x70,
        g: 0xC0,
        b: 0xB1,
    };
    palette[15] = RgbColor {
        r: 0xEA,
        g: 0xEA,
        b: 0xEA,
    };

    let mut idx = 16usize;
    let mut r = 0u8;
    while r < 6 {
        let mut g = 0u8;
        while g < 6 {
            let mut b = 0u8;
            while b < 6 {
                palette[idx] = RgbColor {
                    r: if r == 0 { 0 } else { r * 40 + 55 },
                    g: if g == 0 { 0 } else { g * 40 + 55 },
                    b: if b == 0 { 0 } else { b * 40 + 55 },
                };
                idx += 1;
                b += 1;
            }
            g += 1;
        }
        r += 1;
    }

    while idx < 256 {
        let value = (((idx - 232) as u8) * 10) + 8;
        palette[idx] = RgbColor {
            r: value,
            g: value,
            b: value,
        };
        idx += 1;
    }

    palette
};

pub fn default_palette() -> [RgbColor; 256] {
    DEFAULT_PALETTE
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellColor {
    Palette(u8),
    Rgb(RgbColor),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellStyle {
    pub fg_color: Option<CellColor>,
    pub bg_color: Option<CellColor>,
    pub underline_color: Option<CellColor>,
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub blink: bool,
    pub inverse: bool,
    pub invisible: bool,
    pub strikethrough: bool,
    pub overline: bool,
    pub underline: u8,
    pub underlined: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderColors {
    pub background: RgbColor,
    pub foreground: RgbColor,
    pub palette: [RgbColor; 256],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellWide {
    Narrow,
    Wide,
    SpacerTail,
    SpacerHead,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenTextCell {
    pub wide: CellWide,
    pub graphemes: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenTextRow {
    pub cells: Vec<ScreenTextCell>,
    pub soft_wrapped: bool,
    pub wrap_continuation: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum LinkTarget {
    Uri(String),
    Text { text: String, clicked_byte: usize },
}

/// Inclusive display-cell columns on the current viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkRegion {
    pub row: u16,
    pub start_col: u16,
    pub end_col: u16,
}

#[cfg(test)]
mod test_allocations;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn write_numbered_lines(terminal: &mut Terminal, count: usize) {
        for i in 0..count {
            terminal.write(format!("{i:06}\r\n").as_bytes());
        }
    }

    fn write_padded_lines(terminal: &mut Terminal, count: usize, width: usize) {
        let line = format!("{}\r\n", "x".repeat(width));
        terminal.write(line.repeat(count).as_bytes());
    }

    fn first_rendered_row_text(terminal: &Terminal) -> String {
        let mut render_state = RenderState::new().unwrap();
        render_state.update(terminal).unwrap();
        let mut row_iterator = RowIterator::new().unwrap();
        let mut rows = render_state
            .populate_row_iterator(&mut row_iterator)
            .unwrap();
        let mut row_cells = RowCells::new().unwrap();
        let mut bytes = Vec::new();
        let mut cell_text = String::new();
        let mut row_text = String::new();

        assert!(rows.next());
        let mut cells = rows.populate_cells(&mut row_cells).unwrap();
        while cells.next() {
            cells
                .grapheme_text_into(&mut bytes, &mut cell_text)
                .unwrap();
            row_text.push_str(&cell_text);
        }
        row_text.trim_end().to_owned()
    }

    #[test]
    fn incremental_compression_reports_unsupported_and_tracks_activity() {
        let mut terminal = Terminal::new(80, 24, 20_000_000).unwrap();
        let initial_activity = terminal.compression_activity().unwrap();
        terminal.write(b"hello\r\n");
        assert_ne!(terminal.compression_activity().unwrap(), initial_activity);
        assert_eq!(
            terminal.compress_incremental().unwrap(),
            TerminalCompressionResult::Unsupported
        );
    }

    #[test]
    fn kitty_graphics_sequences_are_silently_ignored_as_no_ops() {
        let mut terminal = Terminal::new(20, 4, 0).unwrap();
        let replies = Arc::new(Mutex::new(Vec::<u8>::new()));
        let captured = Arc::clone(&replies);
        terminal
            .set_write_pty_callback(move |bytes| {
                captured.lock().unwrap().extend_from_slice(bytes);
            })
            .unwrap();
        terminal.enable_kitty_graphics().unwrap();
        terminal.set_kitty_source_forwarding(true).unwrap();

        terminal.write(b"before\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\after");

        assert!(replies.lock().unwrap().is_empty());
        assert_eq!(
            terminal.read_text_viewport((0, 0), (19, 0), false).unwrap(),
            "beforeafter"
        );
        assert_eq!(terminal.kitty_graphics_generation().unwrap(), 0);
        assert!(!terminal.kitty_graphics_may_have_placements().unwrap());
        assert!(terminal.kitty_image_placements().unwrap().is_empty());
        assert_eq!(
            terminal.kitty_image_fingerprints(&[1, 31]).unwrap(),
            vec![None, None]
        );
    }

    #[test]
    fn unicode_width_helpers_match_terminal_layout_rules() {
        assert_eq!(unicode_codepoint_width('A' as u32), 1);
        assert_eq!(unicode_codepoint_width('\u{301}' as u32), 0);
        assert_eq!(unicode_codepoint_width('界' as u32), 2);
        assert_eq!(unicode_codepoint_width(0x11_0000), 1);

        let cases: &[(&[u32], usize, u8)] = &[
            (&[], 0, 0),
            (&['e' as u32, '\u{301}' as u32], 2, 1),
            (&['⚠' as u32, '\u{fe0f}' as u32], 2, 2),
            (&['⚠' as u32, '\u{fe0e}' as u32], 2, 1),
            (&['🇧' as u32, '🇷' as u32], 2, 2),
            (&['👍' as u32, '🏽' as u32], 2, 2),
            (
                &[
                    '👨' as u32,
                    '\u{200d}' as u32,
                    '👩' as u32,
                    '\u{200d}' as u32,
                    '👧' as u32,
                ],
                5,
                2,
            ),
            (&[0x11_0000, 'A' as u32], 1, 1),
        ];
        for &(codepoints, consumed, width) in cases {
            assert_eq!(unicode_grapheme_width(codepoints), (consumed, width));
        }
    }

    #[test]
    fn focus_encoding_matches_expected_sequences() {
        assert_eq!(encode_focus(FocusEvent::Gained).unwrap(), b"\x1b[I");
        assert_eq!(encode_focus(FocusEvent::Lost).unwrap(), b"\x1b[O");
    }

    #[test]
    fn terminal_callbacks_report_pty_responses_and_pwd_changes() {
        let mut terminal = Terminal::new(8, 3, 100).unwrap();
        let responses = Arc::new(Mutex::new(Vec::<u8>::new()));
        let sink = responses.clone();
        terminal
            .set_write_pty_callback(move |bytes| sink.lock().unwrap().extend_from_slice(bytes))
            .unwrap();

        terminal.write(b"\x1b[6n\x1b]7;file:///tmp/herdr\x07");

        let output = responses.lock().unwrap().clone();
        assert!(!output.is_empty());
        assert!(String::from_utf8_lossy(&output).contains('R'));
        assert_eq!(terminal.take_pwd_changes(), [b"file:///tmp/herdr".to_vec()]);
    }

    #[test]
    fn key_and_mouse_encoders_follow_terminal_state() {
        let mut terminal = Terminal::new(80, 24, 0).unwrap();
        terminal.mode_set(1, true).unwrap();
        terminal.write(b"\x1b[>1u\x1b[?1000h\x1b[?1006h");

        assert!(terminal.mode_get(1).unwrap());
        assert_eq!(terminal.kitty_keyboard_flags().unwrap(), 1);
        assert!(terminal.mouse_tracking_enabled().unwrap());

        let mut key_encoder = KeyEncoder::new().unwrap();
        key_encoder.set_from_terminal(&terminal);
        let mut key_event = KeyEvent::new().unwrap();
        key_event.set_action(ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_PRESS);
        key_event.set_key(KEY_A);
        key_event.set_mods(MOD_CTRL | MOD_SHIFT);
        key_event.set_utf8("A");
        key_event.set_unshifted_codepoint('a' as u32);
        let encoded_key = key_encoder.encode(&key_event).unwrap();
        assert_eq!(encoded_key, b"\x1b[97;6u");

        let mut mouse_encoder = MouseEncoder::new().unwrap();
        mouse_encoder.set_from_terminal(&terminal);
        mouse_encoder.set_size(80, 24, 1, 1);
        let mut mouse_event = MouseEvent::new().unwrap();
        mouse_event.set_action(ffi::GhosttyMouseAction_GHOSTTY_MOUSE_ACTION_PRESS);
        mouse_event.set_button(ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_LEFT);
        mouse_event.set_position(0.0, 0.0);
        let encoded_mouse = mouse_encoder.encode(&mouse_event).unwrap();
        assert_eq!(encoded_mouse, b"\x1b[<0;1;1M");
    }

    #[test]
    fn modify_other_keys_query_tracks_mode_two() {
        let mut terminal = Terminal::new(80, 24, 0).unwrap();

        assert!(!terminal.modify_other_keys_enabled().unwrap());
        terminal.write(b"\x1b[>4;2m");
        assert!(terminal.modify_other_keys_enabled().unwrap());
        terminal.write(b"\x1b[>4;0m");
        assert!(!terminal.modify_other_keys_enabled().unwrap());
    }

    #[test]
    fn link_target_includes_offscreen_wrapped_rows() {
        let url = "https://example.com/abcdefghijklmnopqrstuv";
        let mut terminal = Terminal::new(20, 2, 1024 * 1024).unwrap();
        terminal.write(url.as_bytes());
        let expected = LinkTarget::Text {
            text: url.to_owned(),
            clicked_byte: 20,
        };
        assert_eq!(terminal.viewport_link_target(0, 0).unwrap(), Some(expected));
        terminal.scroll_viewport_row(0);
        assert_eq!(
            terminal.viewport_link_target(0, 0).unwrap(),
            Some(LinkTarget::Text {
                text: url.to_owned(),
                clicked_byte: 0
            })
        );
    }

    #[test]
    fn link_target_prefers_explicit_uri_and_does_not_join_hard_lines() {
        let mut terminal = Terminal::new(20, 4, 1024).unwrap();
        terminal.write(b"\x1b]8;;https://example.com/real\x1b\\label\x1b]8;;\x1b\\\r\nhttps://example.com/\r\ntail");
        assert_eq!(
            terminal.viewport_link_target(2, 0).unwrap(),
            Some(LinkTarget::Uri("https://example.com/real".into()))
        );
        assert_eq!(
            terminal.viewport_link_target(1, 2).unwrap(),
            Some(LinkTarget::Text {
                text: "tail".into(),
                clicked_byte: 1
            })
        );
    }

    #[test]
    fn link_target_stops_at_hard_line_when_clicking_last_column() {
        let mut terminal = Terminal::new(20, 3, 1024).unwrap();
        terminal.write(b"https://example.com/x\r\nnot-part-of-url");
        assert_eq!(
            terminal.viewport_link_target(19, 0).unwrap(),
            Some(LinkTarget::Text {
                text: "https://example.com/x".into(),
                clicked_byte: 19
            })
        );
    }

    #[test]
    fn link_target_wide_explicit_label_works_on_both_cell_halves() {
        let mut terminal = Terminal::new(20, 2, 1024).unwrap();
        terminal.write("\x1b]8;;https://example.com/\x1b\\路径\x1b]8;;\x1b\\".as_bytes());
        for col in 0..4 {
            assert_eq!(
                terminal.viewport_link_target(col, 0).unwrap(),
                Some(LinkTarget::Uri("https://example.com/".into()))
            );
        }
    }

    #[test]
    fn link_target_rejects_oversized_token_without_returning_a_prefix() {
        let mut terminal = Terminal::new(100, 4, 16 * 1024 * 1024).unwrap();
        let text = format!("https://example.com/{}", "a".repeat(1_000_000));
        terminal.write(text.as_bytes());
        let target = terminal.viewport_link_target(0, 0).unwrap();
        assert!(target.is_none());
    }

    #[test]
    fn clear_screen_preserves_soft_wrapped_cursor_line_and_rejects_alt_screen() {
        let mut terminal = Terminal::new(10, 4, 1024).unwrap();
        terminal.write(b"history1\r\nhistory2\r\n0123456789prompt> ");
        assert!(terminal.clear_screen());
        assert_eq!(terminal.scrollback_rows().unwrap(), 0);
        assert_eq!(
            terminal.read_text_viewport((0, 0), (9, 1), false).unwrap(),
            "0123456789prompt>"
        );

        terminal.write(b"\x1b[?1049h");
        assert!(!terminal.clear_screen());
    }

    #[test]
    fn terminal_read_text_viewport_unwraps_soft_wrapped_selection() {
        let mut terminal = Terminal::new(5, 3, 0).unwrap();
        terminal.write("1ABCD2EFGH3IJKL".as_bytes());

        let text = terminal.read_text_viewport((0, 1), (2, 2), false).unwrap();
        assert_eq!(text, "2EFGH3IJ");
    }

    #[test]
    fn terminal_extracts_viewport_hyperlink_uri() {
        let mut terminal = Terminal::new(20, 3, 0).unwrap();
        terminal.write(b"\x1b]8;;https://example.com\x1b\\Link\x1b]8;;\x1b\\");

        assert_eq!(
            terminal.viewport_hyperlink_uri(0, 0).unwrap().as_deref(),
            Some("https://example.com")
        );
        assert_eq!(terminal.viewport_hyperlink_uri(4, 0).unwrap(), None);
    }

    #[test]
    fn terminal_read_text_viewport_handles_wide_chars() {
        let mut terminal = Terminal::new(5, 3, 0).unwrap();
        terminal.write("1A⚡".as_bytes());

        let full = terminal.read_text_viewport((0, 0), (3, 0), false).unwrap();
        assert_eq!(full, "1A⚡");

        let through_wide_head = terminal.read_text_viewport((0, 0), (2, 0), false).unwrap();
        assert_eq!(through_wide_head, "1A⚡");

        let wide_only = terminal.read_text_viewport((3, 0), (3, 0), false).unwrap();
        assert_eq!(wide_only, "⚡");
    }

    #[test]
    fn zero_max_scrollback_disables_history() {
        let mut terminal = Terminal::new(80, 3, 0).unwrap();
        write_numbered_lines(&mut terminal, 3000);
        assert_eq!(terminal.scrollback_rows().unwrap(), 0);
    }

    #[test]
    fn max_scrollback_limit_bytes_retains_more_history_for_larger_limits() {
        let mut small = Terminal::new(80, 3, 1_000_000).unwrap();
        let mut large = Terminal::new(80, 3, 10_000_000).unwrap();

        write_padded_lines(&mut small, 1_250, 70);
        write_padded_lines(&mut large, 1_250, 70);

        let small_scrollback = small.scrollback_rows().unwrap();
        let large_scrollback = large.scrollback_rows().unwrap();

        assert!(
            large_scrollback > small_scrollback,
            "expected larger byte limit to retain more history, got small={small_scrollback}, large={large_scrollback}"
        );
    }

    #[test]
    fn large_negative_scroll_delta_reaches_top_of_scrollback() {
        let mut terminal = Terminal::new(80, 3, 1_000_000).unwrap();
        write_numbered_lines(&mut terminal, 1000);

        let before = terminal.scrollbar().unwrap();
        assert!(before.total > before.len);

        terminal.scroll_viewport_bottom();
        terminal.scroll_viewport_delta(-10_000);

        let after = terminal.scrollbar().unwrap();
        assert_eq!(after.offset, 0);
        assert_eq!(after.len, before.len);
    }

    #[test]
    fn absolute_scroll_row_round_trips_and_clamps() {
        let mut terminal = Terminal::new(80, 3, 1_000_000).unwrap();
        write_numbered_lines(&mut terminal, 1000);

        let before = terminal.scrollbar().unwrap();
        let max_row = before.total.saturating_sub(before.len);
        assert!(max_row > 0);

        for row in [0, max_row / 2, max_row, usize::MAX] {
            terminal.scroll_viewport_row(row);
            let after = terminal.scrollbar().unwrap();
            assert_eq!(after.offset, row.min(max_row));
            assert_eq!(after.len, before.len);
        }
    }

    #[test]
    fn deep_scrollback_resize_preserves_unicode_and_hyperlinks() {
        use std::fmt::Write as _;

        let mut terminal = Terminal::new(20, 5, 100_000_000).unwrap();
        let mut input = String::from("\x1b]8;;https://example.com\x1b\\FIRST 🇧🇷\x1b]8;;\x1b\\\r\n");
        for line in 0..70_000 {
            writeln!(input, "{line:05} 👨‍👩‍👧").unwrap();
        }
        terminal.write(input.as_bytes());

        assert!(terminal.scrollback_rows().unwrap() > u16::MAX as usize);
        terminal.scroll_viewport_delta(-100_000);
        assert_eq!(terminal.scrollbar().unwrap().offset, 0);
        assert!(terminal
            .read_text_viewport((0, 0), (19, 0), false)
            .unwrap()
            .starts_with("FIRST 🇧🇷"));
        assert_eq!(
            terminal.viewport_hyperlink_uri(0, 0).unwrap().as_deref(),
            Some("https://example.com")
        );

        terminal.resize(10, 5, 8, 16).unwrap();
        terminal.scroll_viewport_delta(-100_000);
        let metrics = terminal.scrollbar().unwrap();
        assert_eq!(metrics.offset, 0);
        assert_eq!(metrics.len, 5);
        assert!(terminal
            .read_text_viewport((0, 0), (9, 0), false)
            .unwrap()
            .starts_with("FIRST 🇧🇷"));
        assert_eq!(
            terminal.viewport_hyperlink_uri(0, 0).unwrap().as_deref(),
            Some("https://example.com")
        );
    }

    #[test]
    fn raw_resize_preserves_content_without_replaying_terminal_effects() {
        let mut terminal = Terminal::new(20, 6, 100_000).unwrap();
        terminal.write(b"header\r\n\x1b[6;1Htail\x1b[6;18H");
        for (cols, rows) in [(10, 3), (30, 8), (8, 4), (20, 6)] {
            terminal.resize(cols, rows, 8, 16).unwrap();
            terminal.write(b"X");
            let text = terminal
                .read_text_screen(
                    (0, 0),
                    (cols - 1, terminal.total_rows().unwrap() as u32 - 1),
                    false,
                )
                .unwrap();
            assert!(
                text.contains("tail"),
                "lost tail after {cols}x{rows}: {text:?}"
            );
            assert_eq!(text.matches("tail").count(), 1);
            assert!(terminal.cursor_y().unwrap() < rows);
        }
        terminal.write(b"\x1b[2J\x1b[H");
        terminal.resize(12, 3, 8, 16).unwrap();
        assert!(terminal
            .read_text_viewport((0, 0), (11, 2), false)
            .unwrap()
            .trim()
            .is_empty());
    }

    #[test]
    fn clipboard_queries_never_disclose_contents_and_split_writes_complete_once() {
        for suffix in [b"\x07".as_slice(), b"\x1b\\".as_slice()] {
            let bytes = [b"\x1b]52;c;YQBi".as_slice(), suffix].concat();
            for split in 0..=bytes.len() {
                let mut terminal = Terminal::new(10, 3, 0).unwrap();
                let replies = Arc::new(Mutex::new(Vec::new()));
                let captured = Arc::clone(&replies);
                terminal
                    .set_write_pty_callback(move |bytes| {
                        captured.lock().unwrap().extend_from_slice(bytes)
                    })
                    .unwrap();
                terminal.write(&bytes[..split]);
                terminal.write(&bytes[split..]);
                assert_eq!(terminal.take_clipboard_writes(), vec![b"a\0b".to_vec()]);
                terminal.write(b"\x1b]52;c;?\x07\x1b]52;p;YQBi\x07");
                assert!(terminal.take_clipboard_writes().is_empty());
                assert!(replies.lock().unwrap().is_empty());
            }
        }
    }

    #[test]
    fn active_screen_and_cursor_visibility_contract() {
        let mut terminal = Terminal::new(12, 3, 0).unwrap();
        let mut render_state = RenderState::new().unwrap();

        terminal.write(b"primary");
        assert_eq!(terminal.active_screen().unwrap(), ActiveScreen::Primary);
        assert_eq!(
            terminal.read_text_viewport((0, 0), (6, 0), false).unwrap(),
            "primary"
        );

        render_state.update(&terminal).unwrap();
        assert!(render_state.cursor().unwrap().visible);
        terminal.write(b"\x1b[?25l");
        render_state.update(&terminal).unwrap();
        assert!(!render_state.cursor().unwrap().visible);

        terminal.write(b"\x1b[?1049h\x1b[HALT");
        assert_eq!(terminal.active_screen().unwrap(), ActiveScreen::Alternate);
        assert_eq!(
            terminal.read_text_viewport((0, 0), (2, 0), false).unwrap(),
            "ALT"
        );

        terminal.write(b"\x1b[?1049l");
        assert_eq!(terminal.active_screen().unwrap(), ActiveScreen::Primary);
        assert_eq!(
            terminal.read_text_viewport((0, 0), (6, 0), false).unwrap(),
            "primary"
        );
    }

    #[test]
    fn terminal_and_render_state_smoke_test() {
        let mut terminal = Terminal::new(8, 3, 100).unwrap();
        assert_eq!(terminal.cols().unwrap(), 8);
        assert_eq!(terminal.rows().unwrap(), 3);

        terminal.write(b"hello\r\nworld");

        let mut render_state = RenderState::new().unwrap();
        render_state.update(&terminal).unwrap();
        assert_eq!(render_state.cols().unwrap(), 8);
        assert_eq!(render_state.rows().unwrap(), 3);
        assert_ne!(render_state.dirty().unwrap(), Dirty::Clean);

        let mut row_iterator = RowIterator::new().unwrap();
        let mut row_iter = render_state
            .populate_row_iterator(&mut row_iterator)
            .unwrap();
        let mut row_cells = RowCells::new().unwrap();

        let mut found_hello = false;
        let mut found_world = false;
        let mut row_index = 0usize;
        while row_iter.next() {
            let _ = row_iter.dirty().unwrap();
            let mut cells = row_iter.populate_cells(&mut row_cells).unwrap();
            let mut line = String::new();
            while cells.next() {
                let text = cells.grapheme_text().unwrap();
                if text.is_empty() {
                    line.push(' ');
                } else {
                    line.push_str(&text);
                }
            }
            let trimmed = line.trim_end().to_string();
            if row_index == 0 {
                found_hello = trimmed.starts_with("hello");
            }
            if row_index == 1 {
                found_world = trimmed.starts_with("world");
            }
            row_index += 1;
        }

        assert!(found_hello);
        assert!(found_world);

        render_state.set_dirty(Dirty::Clean).unwrap();
        assert_eq!(render_state.dirty().unwrap(), Dirty::Clean);
    }

    #[test]
    fn render_cells_preserve_issue_453_unicode_payload_exactly() {
        const PAYLOAD: &str = "README 👨‍👩‍👧‍👦 🧑‍💻 ✅ ⚡ 漢字 café é 🏳️‍🌈 🚀";
        let mut terminal = Terminal::new(80, 3, 100).unwrap();
        assert!(terminal.mode_get(MODE_GRAPHEME_CLUSTER).unwrap());
        terminal.write(format!("{PAYLOAD}\r\n").as_bytes());

        assert_eq!(first_rendered_row_text(&terminal), PAYLOAD);
    }

    #[test]
    fn grapheme_cluster_mode_is_default_and_survives_full_reset() {
        let mut terminal = Terminal::new(80, 3, 100).unwrap();
        assert!(terminal.mode_get(MODE_GRAPHEME_CLUSTER).unwrap());
        terminal.mode_set(MODE_GRAPHEME_CLUSTER, false).unwrap();
        assert!(!terminal.mode_get(MODE_GRAPHEME_CLUSTER).unwrap());

        terminal.write(b"\x1bc");

        assert!(terminal.mode_get(MODE_GRAPHEME_CLUSTER).unwrap());
    }

    #[test]
    fn screen_text_rows_preserve_wrap_and_grapheme_cells() {
        let mut terminal = Terminal::new(5, 3, 100).unwrap();
        terminal.write("abcdef\r\n界e\u{301}".as_bytes());

        let rows = terminal.screen_text_rows().unwrap();

        assert_eq!(rows.len(), 3);
        assert!(rows[0].soft_wrapped);
        assert!(!rows[0].wrap_continuation);
        assert!(!rows[1].soft_wrapped);
        assert!(rows[1].wrap_continuation);
        assert!(!rows[2].wrap_continuation);
        assert_eq!(rows[2].cells[0].wide, CellWide::Wide);
        assert_eq!(rows[2].cells[0].graphemes, vec!['界' as u32]);
        assert_eq!(rows[2].cells[1].wide, CellWide::SpacerTail);
        assert_eq!(rows[2].cells[2].graphemes, vec!['e' as u32, 0x301]);
    }

    #[test]
    fn owned_cell_graphemes_keep_tight_capacity() {
        let mut terminal = Terminal::new(8, 1, 100).unwrap();
        terminal.write("xe\u{301}".as_bytes());

        let rows = terminal.screen_text_rows().unwrap();
        assert_eq!(rows[0].cells[0].graphemes, vec!['x' as u32]);
        assert_eq!(rows[0].cells[1].graphemes, vec!['e' as u32, 0x301]);
        for cell in &rows[0].cells {
            assert_eq!(cell.graphemes.capacity(), cell.graphemes.len());
        }
    }

    #[test]
    fn borrowed_screen_row_cells_match_owned_cells() {
        let mut terminal = Terminal::new(12, 4, 100).unwrap();
        let content = format!(
            "abcdefghi界Z\r\ne{} X\r\n{}\r\n",
            "\u{301}".repeat(40),
            char::from_u32(KITTY_UNICODE_PLACEHOLDER).unwrap()
        );
        for screen in ["", "\x1b[?1049h"] {
            terminal.write(screen.as_bytes());
            terminal.write(content.as_bytes());
            let expected = terminal.screen_text_rows().unwrap();
            assert!(expected
                .iter()
                .flat_map(|row| &row.cells)
                .any(|cell| cell.graphemes.len() > 4));
            for (y, row) in expected.iter().enumerate() {
                let mut actual = Vec::new();
                terminal
                    .for_each_screen_row_cell(y as u32, |wide, graphemes| {
                        actual.push((wide, graphemes.to_vec()));
                    })
                    .unwrap();
                let expected: Vec<_> = row
                    .cells
                    .iter()
                    .map(|cell| (cell.wide, cell.graphemes.clone()))
                    .collect();
                assert_eq!(actual, expected);
            }
            terminal
                .for_each_screen_row_cell(u32::MAX, |_, _| panic!("out-of-range row"))
                .unwrap();
        }
    }

    #[test]
    fn borrowed_screen_row_allocations_do_not_scale_with_cell_count() {
        for cols in [80, 330] {
            let mut terminal = Terminal::new(cols, 1, 100).unwrap();
            terminal.write("x".repeat(usize::from(cols)).as_bytes());
            let mut cells = 0;
            let (result, allocations) = crate::test_allocations::count(|| {
                terminal.for_each_screen_row_cell(0, |_, graphemes| {
                    std::hint::black_box(graphemes);
                    cells += 1;
                })
            });
            result.unwrap();
            assert_eq!(cells, cols);
            assert!(
                allocations <= 1,
                "one reusable grapheme buffer per ASCII row, got {allocations} allocations for {cols} cells"
            );
        }
    }

    #[test]
    fn render_state_row_dirty_can_be_cleared_independently() {
        let mut terminal = Terminal::new(8, 3, 100).unwrap();
        let mut render_state = RenderState::new().unwrap();

        render_state.update(&terminal).unwrap();
        {
            let mut row_iterator = RowIterator::new().unwrap();
            let mut rows = render_state
                .populate_row_iterator(&mut row_iterator)
                .unwrap();
            while rows.next() {
                rows.clear_dirty().unwrap();
                assert!(!rows.dirty().unwrap());
            }
        }
        {
            let mut iterator = RowIterator::new().unwrap();
            let mut rows = render_state.populate_row_iterator(&mut iterator).unwrap();
            assert_eq!(rows.next_dirty(), Some(0));
            assert_eq!(rows.next_dirty(), Some(1));
            assert_eq!(rows.next_dirty(), Some(2));
            assert_eq!(rows.next_dirty(), None);
        }
        render_state.set_dirty(Dirty::Clean).unwrap();
        assert_eq!(render_state.dirty().unwrap(), Dirty::Clean);
        {
            let mut iterator = RowIterator::new().unwrap();
            let mut rows = render_state.populate_row_iterator(&mut iterator).unwrap();
            assert_eq!(rows.next_dirty(), None);
        }

        terminal.write(b"A");
        render_state.update(&terminal).unwrap();
        assert_eq!(render_state.dirty().unwrap(), Dirty::Partial);

        let mut dirty_rows = 0usize;
        {
            let mut row_iterator = RowIterator::new().unwrap();
            let mut rows = render_state
                .populate_row_iterator(&mut row_iterator)
                .unwrap();
            while let Some(y) = rows.next_dirty() {
                assert_eq!(y, 0);
                assert!(rows.dirty().unwrap());
                dirty_rows += 1;
                rows.clear_dirty().unwrap();
                assert!(!rows.dirty().unwrap());
            }
        }
        assert_eq!(dirty_rows, 1);
        assert_eq!(render_state.dirty().unwrap(), Dirty::Partial);

        render_state.set_dirty(Dirty::Clean).unwrap();
        assert_eq!(render_state.dirty().unwrap(), Dirty::Clean);
    }

    #[test]
    fn row_selection_returns_none_without_selection() {
        let terminal = Terminal::new(8, 3, 100).unwrap();
        let mut render_state = RenderState::new().unwrap();
        render_state.update(&terminal).unwrap();

        let mut row_iterator = RowIterator::new().unwrap();
        let mut rows = render_state
            .populate_row_iterator(&mut row_iterator)
            .unwrap();
        assert!(rows.next());
        assert_eq!(rows.selection().unwrap(), None);
    }

    #[test]
    fn osc52_completes_writes_for_bel_and_st_without_queries() {
        let mut terminal = Terminal::new(10, 5, 0).unwrap();
        terminal.write(b"\x1b]52;c;aGVs");
        assert!(terminal.take_clipboard_writes().is_empty());
        terminal.write(b"bG8=\x07");
        assert_eq!(terminal.take_clipboard_writes(), vec![b"hello".to_vec()]);

        terminal.write(b"\x1b]52;c;d29ybGQ=\x1b\\");
        assert_eq!(terminal.take_clipboard_writes(), vec![b"world".to_vec()]);

        terminal.write(b"\x1b]52;c;?\x07");
        assert!(terminal.take_clipboard_writes().is_empty());

        terminal.write(b"\x1b]52;c;\x07");
        assert!(terminal.take_clipboard_writes().is_empty());
    }

    #[test]
    fn row_cell_basic_data_reports_cell_styling() {
        let mut terminal = Terminal::new(8, 3, 100).unwrap();
        terminal.write(b"\x1b[31mA\x1b[0m");

        let mut render_state = RenderState::new().unwrap();
        render_state.update(&terminal).unwrap();

        let mut row_iterator = RowIterator::new().unwrap();
        let mut rows = render_state
            .populate_row_iterator(&mut row_iterator)
            .unwrap();
        assert!(rows.next());

        let mut row_cells = RowCells::new().unwrap();
        let mut cells = rows.populate_cells(&mut row_cells).unwrap();
        assert!(cells.next());

        let basic = cells.basic_data().unwrap();
        assert_eq!(basic.wide, CellWide::Narrow);
        assert!(basic.has_styling);
        assert_eq!(basic.style.fg_color, Some(CellColor::Palette(1)));
        assert!(!basic.has_hyperlink);
    }
}
