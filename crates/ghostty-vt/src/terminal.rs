//! Core pure-Rust terminal grid, mode state, VT stream parser, and side-effect tracking.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::mem;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, GridCell, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{
    Attr, CharsetIndex, ClearMode, Color as AnsiColor, CursorShape, CursorStyle, Handler,
    Hyperlink, LineClearMode, Mode as AnsiMode, NamedColor, NamedMode, NamedPrivateMode,
    PrivateMode, Rgb as AnsiRgb, ScpCharPath, ScpUpdateMode, StandardCharset, TabulationClearMode,
};
use alacritty_terminal::vte::{Params, ParamsIter, Parser, Perform};
use base64::Engine as _;
use unicode_width::UnicodeWidthChar;

use crate::ffi;
use crate::terminfo::{decode_hex_bytes, lookup_xtgettcap};
use crate::unicode::{check_grapheme_continuation, unicode_codepoint_width};
use crate::{
    default_palette, ActiveScreen, CellColor, CellStyle, CellWide, ColorScheme, CursorVisualStyle,
    Error, KittyImageDescriptor, KittyImagePlacement, RenderColors, RgbColor,
    TerminalCompressionResult, TerminalScrollbar, TERM,
};

/// Unused bit 15 in `alacritty_terminal::term::cell::Flags` repurposed to mark cells
/// where a printable codepoint (including explicit `' '`) was written, distinguishing them
/// from unwritten/erased blank cells and wide-char spacers.
pub(crate) const FLAG_HAS_TEXT: Flags = Flags::from_bits_retain(0x8000);

const MAX_OSC_BYTES: usize = 512 * 1024;
const MAX_DCS_BYTES: usize = 8 * 1024;
const MAX_CLIPBOARD_BYTES: usize = 192 * 1024;
const KITTY_STACK_CAP: usize = 64;

static NEXT_TERMINAL_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug)]
pub(crate) struct TermDimensions {
    pub cols: usize,
    pub screen_lines: usize,
}

impl Dimensions for TermDimensions {
    #[inline]
    fn total_lines(&self) -> usize {
        self.screen_lines
    }

    #[inline]
    fn screen_lines(&self) -> usize {
        self.screen_lines
    }

    #[inline]
    fn columns(&self) -> usize {
        self.cols
    }
}

/// Event sink passed to `alacritty_terminal::Term`. Collects PTY write-backs and bells
/// into a shared queue drained synchronously during `Terminal::write`.
#[derive(Clone, Default)]
pub(crate) struct EventCollector {
    inner: std::sync::Arc<Mutex<CollectedEvents>>,
}

type WritePtyFn = Box<dyn FnMut(&[u8]) + Send>;

#[derive(Default)]
struct CollectedEvents {
    pty_writes: Vec<Vec<u8>>,
    bell_count: u16,
}

impl EventCollector {
    fn drain_into(&self, write_pty: &mut Option<WritePtyFn>, bell_count: &mut u16) {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.bell_count > 0 {
            *bell_count = bell_count.saturating_add(guard.bell_count);
            guard.bell_count = 0;
        }
        if !guard.pty_writes.is_empty() {
            let writes = mem::take(&mut guard.pty_writes);
            drop(guard);
            if let Some(cb) = write_pty.as_mut() {
                for bytes in writes {
                    cb(&bytes);
                }
            }
        }
    }
}

impl EventListener for EventCollector {
    fn send_event(&self, event: Event) {
        match event {
            Event::PtyWrite(text) => {
                let mut guard = self
                    .inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                guard.pty_writes.push(text.into_bytes());
            }
            Event::Bell => {
                let mut guard = self
                    .inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                guard.bell_count = guard.bell_count.saturating_add(1);
            }
            _ => {}
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct KeyEncoderState {
    pub cursor_key_application: bool,
    pub keypad_key_application: bool,
    pub backarrow_key_mode: bool,
    pub ignore_keypad_with_numlock: bool,
    pub alt_esc_prefix: bool,
    pub modify_other_keys_state_2: bool,
    pub kitty_flags: u8,
}

#[derive(Debug, Default)]
enum StreamSeqState {
    #[default]
    Normal,
    Esc,
    Osc {
        buf: Vec<u8>,
        esc: bool,
        overflow: bool,
    },
    Dcs {
        buf: Vec<u8>,
        overflow: bool,
    },
    DcsAfterEsc,
    Apc,
    ApcAfterEsc,
}

pub(crate) struct TerminalInner {
    pub(crate) id: u64,
    pub(crate) term: Term<EventCollector>,
    pub(crate) events: EventCollector,
    parser: Parser,
    seq_state: StreamSeqState,
    preceding_char: Option<char>,
    pub(crate) last_printed: Option<(Line, Column)>,

    pub(crate) logical_cols: u16,
    pub(crate) logical_rows: u16,
    pub(crate) cell_width_px: u32,
    pub(crate) cell_height_px: u32,
    pub(crate) max_scrollback: usize,

    pub(crate) write_activity: u64,
    pub(crate) visual_epoch: u64,
    pub(crate) last_damage_reset_epoch: u64,
    pub(crate) force_full_damage: bool,
    pub(crate) dirty_rows: Vec<bool>,

    // Palette & dynamic colors
    pub(crate) default_palette: [RgbColor; 256],
    pub(crate) active_palette: [RgbColor; 256],
    pub(crate) palette_overridden: [bool; 256],
    pub(crate) fg_override: Option<RgbColor>,
    pub(crate) bg_override: Option<RgbColor>,
    pub(crate) cursor_override: Option<RgbColor>,
    pub(crate) color_scheme: Option<ColorScheme>,

    // Side-effect queues & callbacks
    pub(crate) write_pty: Option<WritePtyFn>,
    pub(crate) bell_count: u16,
    pub(crate) pwd_changes: Vec<Vec<u8>>,
    pub(crate) clipboard_writes: Vec<Vec<u8>>,

    // Modes & input protocol state not natively tracked identically by Term
    pub(crate) reverse_video: bool,
    pub(crate) cursor_blink_mode: bool,
    pub(crate) decscusr_blinking: Option<bool>,
    pub(crate) cursor_hollow: bool,
    pub(crate) reverse_wrap: bool,
    pub(crate) backarrow_key_mode: bool,
    pub(crate) ignore_keypad_with_numlock: bool,
    pub(crate) alt_esc_prefix: bool,
    pub(crate) urgency_hints: bool,
    pub(crate) synchronized_output: bool,
    pub(crate) grapheme_cluster_mode: bool,
    pub(crate) color_scheme_report: bool,
    pub(crate) in_band_size_reports: bool,
    pub(crate) modify_other_keys_state: u8,
    pub(crate) mouse_tracking_mode: ffi::GhosttyMouseTrackingMode,
    pub(crate) mouse_format: ffi::GhosttyMouseFormat,
    pub(crate) mode_mouse_x10: bool,
    pub(crate) mode_mouse_normal: bool,
    pub(crate) mode_mouse_button: bool,
    pub(crate) mode_mouse_any: bool,
    pub(crate) mode_mouse_utf8: bool,
    pub(crate) mode_mouse_sgr: bool,
    pub(crate) mode_mouse_urxvt: bool,
    pub(crate) mode_mouse_sgr_pixels: bool,
    pub(crate) primary_kitty_stack: Vec<u8>,
    pub(crate) alt_kitty_stack: Vec<u8>,

    // Sparse SGR 5/6/25 blink & SGR 53/55 overline tracking (zero-cost unless actually used)
    pub(crate) blink_active: bool,
    pub(crate) primary_blink_cells: HashSet<(i32, usize)>,
    pub(crate) alt_blink_cells: HashSet<(i32, usize)>,
    pub(crate) overline_active: bool,
    pub(crate) primary_overline_cells: HashSet<(i32, usize)>,
    pub(crate) alt_overline_cells: HashSet<(i32, usize)>,

    #[cfg(windows)]
    pub(crate) tracked_grid_line: Option<(ActiveScreen, i32)>,
}

#[inline]
pub(crate) fn scrollback_lines_for_bytes(max_scrollback: usize, cols: u16) -> usize {
    if max_scrollback == 0 {
        0
    } else {
        let bytes_per_row = usize::from(cols.max(1)).saturating_mul(12).max(12);
        (max_scrollback / bytes_per_row).clamp(1_024, 1_000_000)
    }
}

impl TerminalInner {
    fn new(cols: u16, rows: u16, max_scrollback: usize) -> Result<Self, Error> {
        if cols == 0 || rows == 0 {
            return Err(Error(ffi::GhosttyResult_GHOSTTY_INVALID_VALUE));
        }

        let scrolling_history = scrollback_lines_for_bytes(max_scrollback, cols);
        let config = Config {
            scrolling_history,
            kitty_keyboard: true,
            ..Config::default()
        };
        let dims = TermDimensions {
            cols: usize::from(cols).max(alacritty_terminal::term::MIN_COLUMNS),
            screen_lines: usize::from(rows),
        };
        let events = EventCollector::default();
        let term = Term::new(config, &dims, events.clone());
        let palette = default_palette();

        Ok(Self {
            id: NEXT_TERMINAL_ID.fetch_add(1, Ordering::Relaxed),
            term,
            events,
            parser: Parser::new(),
            seq_state: StreamSeqState::Normal,
            preceding_char: None,
            last_printed: None,
            logical_cols: cols,
            logical_rows: rows,
            cell_width_px: 0,
            cell_height_px: 0,
            max_scrollback,
            write_activity: 0,
            visual_epoch: 1,
            last_damage_reset_epoch: 0,
            force_full_damage: true,
            dirty_rows: vec![false; usize::from(rows)],
            default_palette: palette,
            active_palette: palette,
            palette_overridden: [false; 256],
            fg_override: None,
            bg_override: None,
            cursor_override: None,
            color_scheme: None,
            write_pty: None,
            bell_count: 0,
            pwd_changes: Vec::new(),
            clipboard_writes: Vec::new(),
            reverse_video: false,
            cursor_blink_mode: true,
            decscusr_blinking: None,
            cursor_hollow: false,
            reverse_wrap: false,
            backarrow_key_mode: false,
            ignore_keypad_with_numlock: true,
            alt_esc_prefix: true,
            urgency_hints: false,
            synchronized_output: false,
            grapheme_cluster_mode: true,
            color_scheme_report: false,
            in_band_size_reports: false,
            modify_other_keys_state: 0,
            mouse_tracking_mode: ffi::GhosttyMouseTrackingMode_GHOSTTY_MOUSE_TRACKING_NONE,
            mouse_format: ffi::GhosttyMouseFormat_GHOSTTY_MOUSE_FORMAT_X10,
            mode_mouse_x10: false,
            mode_mouse_normal: false,
            mode_mouse_button: false,
            mode_mouse_any: false,
            mode_mouse_utf8: false,
            mode_mouse_sgr: false,
            mode_mouse_urxvt: false,
            mode_mouse_sgr_pixels: false,
            primary_kitty_stack: Vec::new(),
            alt_kitty_stack: Vec::new(),
            blink_active: false,
            primary_blink_cells: HashSet::new(),
            alt_blink_cells: HashSet::new(),
            overline_active: false,
            primary_overline_cells: HashSet::new(),
            alt_overline_cells: HashSet::new(),
            #[cfg(windows)]
            tracked_grid_line: None,
        })
    }

    #[inline]
    pub(crate) fn is_alt_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    #[inline]
    pub(crate) fn active_screen(&self) -> ActiveScreen {
        if self.is_alt_screen() {
            ActiveScreen::Alternate
        } else {
            ActiveScreen::Primary
        }
    }

    #[inline]
    pub(crate) fn current_kitty_flags(&self) -> u8 {
        let stack = if self.is_alt_screen() {
            &self.alt_kitty_stack
        } else {
            &self.primary_kitty_stack
        };
        stack.last().copied().unwrap_or(0)
    }

    #[inline]
    fn active_kitty_stack_mut(&mut self) -> &mut Vec<u8> {
        if self.is_alt_screen() {
            &mut self.alt_kitty_stack
        } else {
            &mut self.primary_kitty_stack
        }
    }

    #[inline]
    pub(crate) fn active_blink_cells(&self) -> &HashSet<(i32, usize)> {
        if self.is_alt_screen() {
            &self.alt_blink_cells
        } else {
            &self.primary_blink_cells
        }
    }

    #[inline]
    fn active_blink_cells_mut(&mut self) -> &mut HashSet<(i32, usize)> {
        if self.is_alt_screen() {
            &mut self.alt_blink_cells
        } else {
            &mut self.primary_blink_cells
        }
    }

    #[inline]
    pub(crate) fn active_overline_cells(&self) -> &HashSet<(i32, usize)> {
        if self.is_alt_screen() {
            &self.alt_overline_cells
        } else {
            &self.primary_overline_cells
        }
    }

    #[inline]
    fn active_overline_cells_mut(&mut self) -> &mut HashSet<(i32, usize)> {
        if self.is_alt_screen() {
            &mut self.alt_overline_cells
        } else {
            &mut self.primary_overline_cells
        }
    }

    #[inline]
    pub(crate) fn bump_visual(&mut self) {
        self.visual_epoch = self.visual_epoch.wrapping_add(1);
    }

    #[inline]
    pub(crate) fn mark_full_damage(&mut self) {
        self.force_full_damage = true;
        self.bump_visual();
    }

    #[inline]
    pub(crate) fn mark_row_dirty(&mut self, line: i32) {
        if self.term.grid().display_offset() != 0 {
            self.mark_full_damage();
            return;
        }
        if let Ok(idx) = usize::try_from(line) {
            if idx < self.dirty_rows.len() {
                self.dirty_rows[idx] = true;
            }
        }
        self.bump_visual();
    }

    fn emit_pty(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if let Some(cb) = self.write_pty.as_mut() {
            cb(bytes);
        }
    }

    fn flush_term_events(&mut self) {
        self.events
            .drain_into(&mut self.write_pty, &mut self.bell_count);
    }

    pub(crate) fn width_px(&self) -> u32 {
        u32::from(self.logical_cols).saturating_mul(self.cell_width_px)
    }

    pub(crate) fn height_px(&self) -> u32 {
        u32::from(self.logical_rows).saturating_mul(self.cell_height_px)
    }

    fn emit_in_band_size_report(&mut self) {
        let mut msg = String::with_capacity(32);
        let _ = write!(
            msg,
            "\x1b[48;{};{};{};{}t",
            self.logical_rows,
            self.logical_cols,
            self.height_px(),
            self.width_px()
        );
        self.emit_pty(msg.as_bytes());
    }

    fn write(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.write_activity = self.write_activity.wrapping_add(1);

        let mut i = 0usize;
        while i < bytes.len() {
            match &mut self.seq_state {
                StreamSeqState::Normal => {
                    // Fast slice scan up to the next ESC (0x1B).
                    let start = i;
                    while i < bytes.len() && bytes[i] != 0x1b {
                        i += 1;
                    }
                    if i > start {
                        self.advance_vte(&bytes[start..i]);
                    }
                    if i < bytes.len() && bytes[i] == 0x1b {
                        self.seq_state = StreamSeqState::Esc;
                        i += 1;
                    }
                }
                StreamSeqState::Esc => {
                    let b = bytes[i];
                    i += 1;
                    match b {
                        0x1b => {
                            // Repeated ESC stays in Esc state.
                        }
                        b']' => {
                            // Cancel any partial CSI/ESC in vte::Parser before entering OSC.
                            self.parser = Parser::new();
                            self.last_printed = None;
                            self.seq_state = StreamSeqState::Osc {
                                buf: Vec::new(),
                                esc: false,
                                overflow: false,
                            };
                        }
                        b'P' => {
                            self.parser = Parser::new();
                            self.last_printed = None;
                            self.seq_state = StreamSeqState::Dcs {
                                buf: Vec::new(),
                                overflow: false,
                            };
                        }
                        b'_' => {
                            // APC (including Kitty Graphics \x1b_G...\x1b\\): silently discard.
                            self.parser = Parser::new();
                            self.last_printed = None;
                            self.seq_state = StreamSeqState::Apc;
                        }
                        _ => {
                            self.seq_state = StreamSeqState::Normal;
                            self.advance_vte(&[0x1b, b]);
                        }
                    }
                }
                StreamSeqState::Osc { buf, esc, overflow } => {
                    let b = bytes[i];
                    i += 1;
                    if *esc {
                        if b == b'\\' {
                            let completed = if *overflow {
                                None
                            } else {
                                Some(mem::take(buf))
                            };
                            self.seq_state = StreamSeqState::Normal;
                            if let Some(payload) = completed {
                                self.handle_osc(&payload, b"\x1b\\");
                            }
                        } else if b == 0x1b {
                            // ESC ESC aborts the OSC without dispatching and starts a fresh Esc.
                            self.seq_state = StreamSeqState::Esc;
                        } else {
                            // ESC followed by anything other than `\` aborts OSC without
                            // dispatching and processes `ESC b` as a new escape sequence.
                            self.seq_state = StreamSeqState::Normal;
                            self.reprocess_esc_byte(b);
                        }
                    } else {
                        match b {
                            0x07 => {
                                let completed = if *overflow {
                                    None
                                } else {
                                    Some(mem::take(buf))
                                };
                                self.seq_state = StreamSeqState::Normal;
                                if let Some(payload) = completed {
                                    self.handle_osc(&payload, b"\x07");
                                }
                            }
                            0x18 | 0x1a => {
                                // CAN / SUB cancel OSC without dispatching.
                                self.seq_state = StreamSeqState::Normal;
                            }
                            0x1b => {
                                *esc = true;
                            }
                            _ => {
                                if !*overflow {
                                    if buf.len() < MAX_OSC_BYTES {
                                        buf.push(b);
                                    } else {
                                        buf.clear();
                                        *overflow = true;
                                    }
                                }
                            }
                        }
                    }
                }
                StreamSeqState::Dcs { buf, overflow } => {
                    let b = bytes[i];
                    i += 1;
                    match b {
                        0x18 | 0x1a | 0x9c => {
                            // CAN, SUB, and 8-bit 0x9C cancel DCS without dispatching 7-bit XTGETTCAP.
                            self.seq_state = StreamSeqState::Normal;
                        }
                        0x1b => {
                            // Ghostty unhooks DCS immediately upon seeing ESC before `\`.
                            let completed = if *overflow {
                                None
                            } else {
                                Some(mem::take(buf))
                            };
                            self.seq_state = StreamSeqState::DcsAfterEsc;
                            if let Some(payload) = completed {
                                self.handle_dcs(&payload);
                            }
                        }
                        _ => {
                            if !*overflow {
                                if buf.len() < MAX_DCS_BYTES {
                                    buf.push(b);
                                } else {
                                    buf.clear();
                                    *overflow = true;
                                }
                            }
                        }
                    }
                }
                StreamSeqState::DcsAfterEsc => {
                    let b = bytes[i];
                    i += 1;
                    if b == b'\\' {
                        self.seq_state = StreamSeqState::Normal;
                    } else if b == 0x1b {
                        self.seq_state = StreamSeqState::Esc;
                    } else {
                        self.seq_state = StreamSeqState::Normal;
                        self.reprocess_esc_byte(b);
                    }
                }
                StreamSeqState::Apc => {
                    // Fast skip until ESC / CAN / SUB / 0x9C.
                    while i < bytes.len() {
                        let b = bytes[i];
                        i += 1;
                        match b {
                            0x18 | 0x1a | 0x9c => {
                                self.seq_state = StreamSeqState::Normal;
                                break;
                            }
                            0x1b => {
                                self.seq_state = StreamSeqState::ApcAfterEsc;
                                break;
                            }
                            _ => {}
                        }
                    }
                }
                StreamSeqState::ApcAfterEsc => {
                    let b = bytes[i];
                    i += 1;
                    if b == b'\\' {
                        self.seq_state = StreamSeqState::Normal;
                    } else if b == 0x1b {
                        self.seq_state = StreamSeqState::Esc;
                    } else {
                        self.seq_state = StreamSeqState::Normal;
                        self.reprocess_esc_byte(b);
                    }
                }
            }
        }
    }

    fn reprocess_esc_byte(&mut self, b: u8) {
        match b {
            b']' => {
                self.parser = Parser::new();
                self.last_printed = None;
                self.seq_state = StreamSeqState::Osc {
                    buf: Vec::new(),
                    esc: false,
                    overflow: false,
                };
            }
            b'P' => {
                self.parser = Parser::new();
                self.last_printed = None;
                self.seq_state = StreamSeqState::Dcs {
                    buf: Vec::new(),
                    overflow: false,
                };
            }
            b'_' => {
                self.parser = Parser::new();
                self.last_printed = None;
                self.seq_state = StreamSeqState::Apc;
            }
            _ => {
                self.advance_vte(&[0x1b, b]);
            }
        }
    }

    fn advance_vte(&mut self, slice: &[u8]) {
        let mut parser = mem::replace(&mut self.parser, Parser::new());
        {
            let mut performer = VtPerformer { inner: self };
            parser.advance(&mut performer, slice);
        }
        self.parser = parser;
        self.flush_term_events();
    }

    fn handle_dcs(&mut self, payload: &[u8]) {
        // XTGETTCAP: DCS + q <hex>[;<hex>...] ST
        // Matches Ghostty: emit 1+r responses for known capabilities and silently skip unknown/invalid keys.
        if let Some(rest) = payload.strip_prefix(b"+q") {
            if rest.is_empty() {
                return;
            }
            for item in rest.split(|&b| b == b';') {
                if let Some(reply) =
                    decode_hex_bytes(item).and_then(|name| lookup_xtgettcap(&name, TERM))
                {
                    self.emit_pty(&reply);
                }
            }
        }
    }

    fn handle_osc(&mut self, payload: &[u8], terminator: &[u8]) {
        if payload.is_empty() {
            return;
        }
        let (cmd, rest) = match payload.iter().position(|&b| b == b';') {
            Some(pos) => (&payload[..pos], Some(&payload[pos + 1..])),
            None => (payload, None),
        };

        match cmd {
            b"0" | b"2" => {
                if let Some(raw_title) = rest {
                    if let Ok(title) = std::str::from_utf8(raw_title) {
                        Handler::set_title(&mut self.term, Some(title.to_owned()));
                        self.flush_term_events();
                    }
                }
            }
            b"4" => {
                let Some(rest) = rest else {
                    return;
                };
                let parts: Vec<&[u8]> = rest.split(|&b| b == b';').collect();
                if parts.len() < 2 || !parts.len().is_multiple_of(2) {
                    return;
                }
                let mut reply = Vec::new();
                for pair in parts.chunks_exact(2) {
                    let Some(idx) = parse_u16_ascii(pair[0]).filter(|&n| n <= 255) else {
                        continue;
                    };
                    let idx_usize = usize::from(idx);
                    if pair[1] == b"?" {
                        let c = self.active_palette[idx_usize];
                        write_osc_color_reply(&mut reply, &format!("4;{idx}"), c, terminator);
                    } else if let Some(rgb) = parse_x_color(pair[1]) {
                        self.active_palette[idx_usize] = rgb;
                        self.palette_overridden[idx_usize] = true;
                        Handler::set_color(
                            &mut self.term,
                            idx_usize,
                            AnsiRgb {
                                r: rgb.r,
                                g: rgb.g,
                                b: rgb.b,
                            },
                        );
                        self.mark_full_damage();
                    }
                }
                if !reply.is_empty() {
                    self.emit_pty(&reply);
                }
            }
            b"7" => {
                if let Some(uri) = rest.filter(|u| !u.is_empty()) {
                    self.pwd_changes.push(uri.to_vec());
                }
            }
            b"9" => {
                // ConEmu / Windows Terminal CWD report: OSC 9 ; 9 ; <path> ST
                if let Some(path) = rest
                    .and_then(|r| r.strip_prefix(b"9;"))
                    .filter(|p| !p.is_empty())
                {
                    self.pwd_changes.push(path.to_vec());
                }
            }
            b"1337" => {
                // iTerm2 CWD report: OSC 1337 ; CurrentDir=<path> ST
                if let Some(path) = rest
                    .and_then(|r| r.strip_prefix(b"CurrentDir="))
                    .filter(|p| !p.is_empty())
                {
                    self.pwd_changes.push(path.to_vec());
                }
            }
            b"8" => {
                let Some(rest) = rest else {
                    return;
                };
                let Some(semi) = rest.iter().position(|&b| b == b';') else {
                    return;
                };
                let link_params = &rest[..semi];
                let uri_bytes = &rest[semi + 1..];
                if uri_bytes.is_empty() {
                    Handler::set_hyperlink(&mut self.term, None);
                    return;
                }
                let Ok(uri_str) = std::str::from_utf8(uri_bytes) else {
                    return;
                };
                let id = link_params
                    .split(|&b| b == b':')
                    .find_map(|kv| kv.strip_prefix(b"id="))
                    .and_then(|v| std::str::from_utf8(v).ok())
                    .map(str::to_owned);
                Handler::set_hyperlink(
                    &mut self.term,
                    Some(Hyperlink {
                        id,
                        uri: uri_str.to_owned(),
                    }),
                );
            }
            b"10" | b"11" | b"12" => {
                let Some(rest) = rest else {
                    return;
                };
                let Some(mut dynamic_code) = parse_u16_ascii(cmd) else {
                    return;
                };
                let mut reply = Vec::new();
                for spec in rest.split(|&b| b == b';') {
                    if dynamic_code > 12 {
                        break;
                    }
                    if spec == b"?" {
                        let queried = match dynamic_code {
                            10 => self.fg_override,
                            11 => self.bg_override,
                            12 => self.cursor_override.or(self.fg_override),
                            _ => None,
                        };
                        if let Some(c) = queried {
                            write_osc_color_reply(
                                &mut reply,
                                &dynamic_code.to_string(),
                                c,
                                terminator,
                            );
                        }
                    } else if let Some(rgb) = parse_x_color(spec) {
                        match dynamic_code {
                            10 => {
                                self.fg_override = Some(rgb);
                                Handler::set_color(
                                    &mut self.term,
                                    NamedColor::Foreground as usize,
                                    AnsiRgb {
                                        r: rgb.r,
                                        g: rgb.g,
                                        b: rgb.b,
                                    },
                                );
                            }
                            11 => {
                                self.bg_override = Some(rgb);
                                Handler::set_color(
                                    &mut self.term,
                                    NamedColor::Background as usize,
                                    AnsiRgb {
                                        r: rgb.r,
                                        g: rgb.g,
                                        b: rgb.b,
                                    },
                                );
                            }
                            12 => {
                                self.cursor_override = Some(rgb);
                                Handler::set_color(
                                    &mut self.term,
                                    NamedColor::Cursor as usize,
                                    AnsiRgb {
                                        r: rgb.r,
                                        g: rgb.g,
                                        b: rgb.b,
                                    },
                                );
                            }
                            _ => {}
                        }
                        self.mark_full_damage();
                    }
                    dynamic_code += 1;
                }
                if !reply.is_empty() {
                    self.emit_pty(&reply);
                }
            }
            b"52" => {
                let Some(rest) = rest else {
                    return;
                };
                let (selection, b64) = match rest.iter().position(|&b| b == b';') {
                    Some(pos) => (&rest[..pos], &rest[pos + 1..]),
                    None => return,
                };
                // Ghostty accepts clipboard selection "" or containing 'c', rejecting primary-only "p".
                if !selection.is_empty() && !selection.contains(&b'c') {
                    return;
                }
                if b64.is_empty() || b64 == b"?" {
                    return;
                }
                if let Some(decoded) = decode_osc52_base64(b64) {
                    if !decoded.is_empty() && decoded.len() <= MAX_CLIPBOARD_BYTES {
                        self.clipboard_writes.push(decoded);
                    }
                }
            }
            b"104" => match rest {
                None | Some(b"") => {
                    self.active_palette = self.default_palette;
                    self.palette_overridden = [false; 256];
                    for i in 0..256 {
                        Handler::reset_color(&mut self.term, i);
                    }
                    self.mark_full_damage();
                }
                Some(list) => {
                    let mut changed = false;
                    for part in list.split(|&b| b == b';') {
                        if let Some(idx) = parse_u16_ascii(part).filter(|&n| n <= 255) {
                            let i = usize::from(idx);
                            self.active_palette[i] = self.default_palette[i];
                            self.palette_overridden[i] = false;
                            Handler::reset_color(&mut self.term, i);
                            changed = true;
                        }
                    }
                    if changed {
                        self.mark_full_damage();
                    }
                }
            },
            b"110" => {
                if self.fg_override.take().is_some() {
                    Handler::reset_color(&mut self.term, NamedColor::Foreground as usize);
                    self.mark_full_damage();
                }
            }
            b"111" => {
                if self.bg_override.take().is_some() {
                    Handler::reset_color(&mut self.term, NamedColor::Background as usize);
                    self.mark_full_damage();
                }
            }
            b"112" if self.cursor_override.take().is_some() => {
                Handler::reset_color(&mut self.term, NamedColor::Cursor as usize);
                self.mark_full_damage();
            }
            _ => {}
        }
    }

    pub(crate) fn get_mode_internal(&self, mode: u16, ansi: bool) -> Option<bool> {
        let tm = *self.term.mode();
        if ansi {
            return match mode {
                2 => Some(false), // KAM
                4 => Some(tm.contains(TermMode::INSERT)),
                12 => Some(false), // SRM
                20 => Some(tm.contains(TermMode::LINE_FEED_NEW_LINE)),
                _ => None,
            };
        }
        match mode {
            1 => Some(tm.contains(TermMode::APP_CURSOR)),
            3 => Some(false),
            5 => Some(self.reverse_video),
            6 => Some(tm.contains(TermMode::ORIGIN)),
            7 => Some(tm.contains(TermMode::LINE_WRAP)),
            9 => Some(self.mode_mouse_x10),
            12 => Some(self.cursor_blink_mode),
            25 => Some(tm.contains(TermMode::SHOW_CURSOR)),
            45 => Some(self.reverse_wrap),
            66 => Some(tm.contains(TermMode::APP_KEYPAD)),
            67 => Some(self.backarrow_key_mode),
            1000 => Some(self.mode_mouse_normal),
            1002 => Some(self.mode_mouse_button),
            1003 => Some(self.mode_mouse_any),
            1004 => Some(tm.contains(TermMode::FOCUS_IN_OUT)),
            1005 => Some(self.mode_mouse_utf8),
            1006 => Some(self.mode_mouse_sgr),
            1007 => Some(tm.contains(TermMode::ALTERNATE_SCROLL)),
            1015 => Some(self.mode_mouse_urxvt),
            1016 => Some(self.mode_mouse_sgr_pixels),
            1035 => Some(self.ignore_keypad_with_numlock),
            1036 => Some(self.alt_esc_prefix),
            1042 => Some(self.urgency_hints),
            47 | 1047 | 1049 => Some(tm.contains(TermMode::ALT_SCREEN)),
            1048 => Some(false),
            2004 => Some(tm.contains(TermMode::BRACKETED_PASTE)),
            2026 => Some(self.synchronized_output),
            2027 => Some(self.grapheme_cluster_mode),
            2031 => Some(self.color_scheme_report),
            2048 => Some(self.in_band_size_reports),
            _ => None,
        }
    }

    pub(crate) fn set_private_mode_num(&mut self, mode: u16, enabled: bool, from_stream: bool) {
        match mode {
            1 => {
                self.apply_term_private_mode(NamedPrivateMode::CursorKeys, enabled);
            }
            3 => {
                self.apply_term_private_mode(NamedPrivateMode::ColumnMode, enabled);
                self.mark_full_damage();
            }
            5 => {
                if self.reverse_video != enabled {
                    self.reverse_video = enabled;
                    self.mark_full_damage();
                }
            }
            6 => {
                self.apply_term_private_mode(NamedPrivateMode::Origin, enabled);
                self.bump_visual();
            }
            7 => {
                self.apply_term_private_mode(NamedPrivateMode::LineWrap, enabled);
            }
            9 => {
                self.mode_mouse_x10 = enabled;
                if enabled {
                    self.mouse_tracking_mode =
                        ffi::GhosttyMouseTrackingMode_GHOSTTY_MOUSE_TRACKING_X10;
                    Handler::set_private_mode(
                        &mut self.term,
                        PrivateMode::Named(NamedPrivateMode::ReportMouseClicks),
                    );
                } else {
                    self.mouse_tracking_mode =
                        ffi::GhosttyMouseTrackingMode_GHOSTTY_MOUSE_TRACKING_NONE;
                    Handler::unset_private_mode(
                        &mut self.term,
                        PrivateMode::Named(NamedPrivateMode::ReportMouseClicks),
                    );
                }
            }
            12 => {
                self.cursor_blink_mode = enabled;
                self.apply_term_private_mode(NamedPrivateMode::BlinkingCursor, enabled);
                self.bump_visual();
            }
            25 => {
                self.apply_term_private_mode(NamedPrivateMode::ShowCursor, enabled);
                self.bump_visual();
            }
            45 => {
                self.reverse_wrap = enabled;
            }
            66 => {
                if enabled {
                    Handler::set_keypad_application_mode(&mut self.term);
                } else {
                    Handler::unset_keypad_application_mode(&mut self.term);
                }
            }
            67 => {
                self.backarrow_key_mode = enabled;
            }
            1000 => {
                self.mode_mouse_normal = enabled;
                self.update_mouse_tracking(
                    ffi::GhosttyMouseTrackingMode_GHOSTTY_MOUSE_TRACKING_NORMAL,
                    NamedPrivateMode::ReportMouseClicks,
                    enabled,
                );
            }
            1002 => {
                self.mode_mouse_button = enabled;
                self.update_mouse_tracking(
                    ffi::GhosttyMouseTrackingMode_GHOSTTY_MOUSE_TRACKING_BUTTON,
                    NamedPrivateMode::ReportCellMouseMotion,
                    enabled,
                );
            }
            1003 => {
                self.mode_mouse_any = enabled;
                self.update_mouse_tracking(
                    ffi::GhosttyMouseTrackingMode_GHOSTTY_MOUSE_TRACKING_ANY,
                    NamedPrivateMode::ReportAllMouseMotion,
                    enabled,
                );
            }
            1004 => {
                self.apply_term_private_mode(NamedPrivateMode::ReportFocusInOut, enabled);
            }
            1005 => {
                self.mode_mouse_utf8 = enabled;
                self.update_mouse_format(
                    ffi::GhosttyMouseFormat_GHOSTTY_MOUSE_FORMAT_UTF8,
                    enabled,
                );
                self.apply_term_private_mode(NamedPrivateMode::Utf8Mouse, enabled);
            }
            1006 => {
                self.mode_mouse_sgr = enabled;
                self.update_mouse_format(ffi::GhosttyMouseFormat_GHOSTTY_MOUSE_FORMAT_SGR, enabled);
                self.apply_term_private_mode(NamedPrivateMode::SgrMouse, enabled);
            }
            1007 => {
                self.apply_term_private_mode(NamedPrivateMode::AlternateScroll, enabled);
            }
            1015 => {
                self.mode_mouse_urxvt = enabled;
                self.update_mouse_format(
                    ffi::GhosttyMouseFormat_GHOSTTY_MOUSE_FORMAT_URXVT,
                    enabled,
                );
            }
            1016 => {
                self.mode_mouse_sgr_pixels = enabled;
                self.update_mouse_format(
                    ffi::GhosttyMouseFormat_GHOSTTY_MOUSE_FORMAT_SGR_PIXELS,
                    enabled,
                );
            }
            1035 => {
                self.ignore_keypad_with_numlock = enabled;
            }
            1036 => {
                self.alt_esc_prefix = enabled;
            }
            1042 => {
                self.urgency_hints = enabled;
                self.apply_term_private_mode(NamedPrivateMode::UrgencyHints, enabled);
            }
            47 | 1047 => {
                let was_alt = self.is_alt_screen();
                if enabled != was_alt {
                    if !enabled {
                        self.alt_blink_cells.clear();
                        self.alt_overline_cells.clear();
                    }
                    self.term.swap_alt();
                    self.mark_full_damage();
                }
            }
            1048 => {
                if enabled {
                    Handler::save_cursor_position(&mut self.term);
                } else {
                    Handler::restore_cursor_position(&mut self.term);
                    self.bump_visual();
                }
            }
            1049 => {
                let was_alt = self.is_alt_screen();
                if enabled {
                    if !was_alt {
                        self.alt_blink_cells.clear();
                        self.alt_overline_cells.clear();
                    }
                    self.apply_term_private_mode(
                        NamedPrivateMode::SwapScreenAndSetRestoreCursor,
                        true,
                    );
                    self.mark_full_damage();
                } else if was_alt {
                    self.alt_blink_cells.clear();
                    self.alt_overline_cells.clear();
                    self.apply_term_private_mode(
                        NamedPrivateMode::SwapScreenAndSetRestoreCursor,
                        false,
                    );
                    self.mark_full_damage();
                }
            }
            2004 => {
                self.apply_term_private_mode(NamedPrivateMode::BracketedPaste, enabled);
            }
            2026 => {
                self.synchronized_output = enabled;
            }
            2027 => {
                self.grapheme_cluster_mode = enabled;
                self.last_printed = None;
            }
            2031 => {
                self.color_scheme_report = enabled;
            }
            2048 => {
                self.in_band_size_reports = enabled;
                if enabled && from_stream {
                    self.emit_in_band_size_report();
                }
            }
            _ => {
                if enabled {
                    Handler::set_private_mode(&mut self.term, PrivateMode::Unknown(mode));
                } else {
                    Handler::unset_private_mode(&mut self.term, PrivateMode::Unknown(mode));
                }
            }
        }
    }

    fn apply_term_private_mode(&mut self, mode: NamedPrivateMode, enabled: bool) {
        if enabled {
            Handler::set_private_mode(&mut self.term, PrivateMode::Named(mode));
        } else {
            Handler::unset_private_mode(&mut self.term, PrivateMode::Named(mode));
        }
    }

    fn update_mouse_tracking(
        &mut self,
        target: ffi::GhosttyMouseTrackingMode,
        term_mode: NamedPrivateMode,
        enabled: bool,
    ) {
        if enabled {
            self.mouse_tracking_mode = target;
            Handler::set_private_mode(&mut self.term, PrivateMode::Named(term_mode));
        } else {
            self.mouse_tracking_mode = ffi::GhosttyMouseTrackingMode_GHOSTTY_MOUSE_TRACKING_NONE;
            Handler::unset_private_mode(&mut self.term, PrivateMode::Named(term_mode));
        }
    }

    fn update_mouse_format(&mut self, target: ffi::GhosttyMouseFormat, enabled: bool) {
        if enabled {
            self.mouse_format = target;
        } else {
            self.mouse_format = ffi::GhosttyMouseFormat_GHOSTTY_MOUSE_FORMAT_X10;
        }
    }

    fn reset_all_state(&mut self) {
        Handler::reset_state(&mut self.term);
        self.preceding_char = None;
        self.last_printed = None;
        self.active_palette = self.default_palette;
        self.palette_overridden = [false; 256];
        self.fg_override = None;
        self.bg_override = None;
        self.cursor_override = None;
        self.reverse_video = false;
        self.cursor_blink_mode = true;
        self.decscusr_blinking = None;
        self.cursor_hollow = false;
        self.reverse_wrap = false;
        self.backarrow_key_mode = false;
        self.ignore_keypad_with_numlock = true;
        self.alt_esc_prefix = true;
        self.urgency_hints = false;
        self.synchronized_output = false;
        self.grapheme_cluster_mode = true;
        self.color_scheme_report = false;
        self.in_band_size_reports = false;
        self.modify_other_keys_state = 0;
        self.mouse_tracking_mode = ffi::GhosttyMouseTrackingMode_GHOSTTY_MOUSE_TRACKING_NONE;
        self.mouse_format = ffi::GhosttyMouseFormat_GHOSTTY_MOUSE_FORMAT_X10;
        self.mode_mouse_x10 = false;
        self.mode_mouse_normal = false;
        self.mode_mouse_button = false;
        self.mode_mouse_any = false;
        self.mode_mouse_utf8 = false;
        self.mode_mouse_sgr = false;
        self.mode_mouse_urxvt = false;
        self.mode_mouse_sgr_pixels = false;
        self.primary_kitty_stack.clear();
        self.alt_kitty_stack.clear();
        self.blink_active = false;
        self.primary_blink_cells.clear();
        self.alt_blink_cells.clear();
        self.overline_active = false;
        self.primary_overline_cells.clear();
        self.alt_overline_cells.clear();
        self.mark_full_damage();
    }

    pub(crate) fn render_colors(&self) -> RenderColors {
        let mut fg = self.fg_override.unwrap_or(RgbColor {
            r: 255,
            g: 255,
            b: 255,
        });
        let mut bg = self.bg_override.unwrap_or(RgbColor { r: 0, g: 0, b: 0 });
        if self.reverse_video {
            mem::swap(&mut fg, &mut bg);
        }
        RenderColors {
            background: bg,
            foreground: fg,
            palette: self.active_palette,
        }
    }

    pub(crate) fn cursor_visual_style_and_blink(&self) -> (CursorVisualStyle, bool) {
        if self.cursor_hollow {
            let blinking =
                self.decscusr_blinking.unwrap_or(self.cursor_blink_mode) && self.cursor_blink_mode;
            return (CursorVisualStyle::BlockHollow, blinking);
        }
        let shape = self.term.cursor_style().shape;
        let visual = match shape {
            CursorShape::Block => CursorVisualStyle::Block,
            CursorShape::Underline => CursorVisualStyle::Underline,
            CursorShape::Beam => CursorVisualStyle::Bar,
            CursorShape::HollowBlock => CursorVisualStyle::BlockHollow,
            CursorShape::Hidden => CursorVisualStyle::Block,
        };
        let blinking = match self.decscusr_blinking {
            Some(dec_blink) => dec_blink && self.cursor_blink_mode,
            None => self.cursor_blink_mode,
        };
        (visual, blinking)
    }

    pub(crate) fn cell_style_at(&self, cell: &Cell, line: i32, col: usize) -> (CellStyle, bool) {
        let flags = cell.flags;
        let fg_color = convert_ansi_color(cell.fg, true);
        let bg_color = convert_ansi_color(cell.bg, false);
        let underline_color = cell
            .underline_color()
            .and_then(|c| convert_ansi_color(c, true));

        let underline: u8 = if flags.contains(Flags::DOUBLE_UNDERLINE) {
            2
        } else if flags.contains(Flags::UNDERCURL) {
            3
        } else if flags.contains(Flags::DOTTED_UNDERLINE) {
            4
        } else if flags.contains(Flags::DASHED_UNDERLINE) {
            5
        } else if flags.contains(Flags::UNDERLINE) {
            1
        } else {
            0
        };

        let bold = flags.contains(Flags::BOLD);
        let italic = flags.contains(Flags::ITALIC);
        let faint = flags.contains(Flags::DIM);
        let blink = {
            let set = self.active_blink_cells();
            !set.is_empty() && set.contains(&(line, col))
        };
        let inverse = flags.contains(Flags::INVERSE);
        let invisible = flags.contains(Flags::HIDDEN);
        let strikethrough = flags.contains(Flags::STRIKEOUT);
        let overline = {
            let set = self.active_overline_cells();
            !set.is_empty() && set.contains(&(line, col))
        };
        let underlined = underline != 0;

        let has_styling = fg_color.is_some()
            || bg_color.is_some()
            || underline_color.is_some()
            || bold
            || italic
            || faint
            || blink
            || inverse
            || invisible
            || strikethrough
            || overline
            || underlined;

        (
            CellStyle {
                fg_color,
                bg_color,
                underline_color,
                bold,
                italic,
                faint,
                blink,
                inverse,
                invisible,
                strikethrough,
                overline,
                underline,
                underlined,
            },
            has_styling,
        )
    }
}

#[inline]
pub(crate) fn cell_wide_from_flags(flags: Flags) -> CellWide {
    if flags.contains(Flags::WIDE_CHAR) {
        CellWide::Wide
    } else if flags.contains(Flags::WIDE_CHAR_SPACER) {
        CellWide::SpacerTail
    } else if flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
        CellWide::SpacerHead
    } else {
        CellWide::Narrow
    }
}

#[inline]
fn convert_ansi_color(color: AnsiColor, is_fg: bool) -> Option<CellColor> {
    match color {
        AnsiColor::Spec(rgb) => Some(CellColor::Rgb(RgbColor {
            r: rgb.r,
            g: rgb.g,
            b: rgb.b,
        })),
        AnsiColor::Indexed(idx) => Some(CellColor::Palette(idx)),
        AnsiColor::Named(named) => match named {
            NamedColor::Black => Some(CellColor::Palette(0)),
            NamedColor::Red => Some(CellColor::Palette(1)),
            NamedColor::Green => Some(CellColor::Palette(2)),
            NamedColor::Yellow => Some(CellColor::Palette(3)),
            NamedColor::Blue => Some(CellColor::Palette(4)),
            NamedColor::Magenta => Some(CellColor::Palette(5)),
            NamedColor::Cyan => Some(CellColor::Palette(6)),
            NamedColor::White => Some(CellColor::Palette(7)),
            NamedColor::BrightBlack | NamedColor::DimBlack => Some(CellColor::Palette(8)),
            NamedColor::BrightRed => Some(CellColor::Palette(9)),
            NamedColor::BrightGreen => Some(CellColor::Palette(10)),
            NamedColor::BrightYellow => Some(CellColor::Palette(11)),
            NamedColor::BrightBlue => Some(CellColor::Palette(12)),
            NamedColor::BrightMagenta => Some(CellColor::Palette(13)),
            NamedColor::BrightCyan => Some(CellColor::Palette(14)),
            NamedColor::BrightWhite => Some(CellColor::Palette(15)),
            NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::DimForeground
                if is_fg =>
            {
                None
            }
            NamedColor::Background if !is_fg => None,
            _ => None,
        },
    }
}

struct VtPerformer<'a> {
    inner: &'a mut TerminalInner,
}

impl VtPerformer<'_> {
    #[inline]
    fn call_linefeed(&mut self) {
        let pre_hist = self.inner.term.grid().history_size();
        let pre_line = self.inner.term.grid().cursor.point.line.0;
        Handler::linefeed(&mut self.inner.term);
        let post_hist = self.inner.term.grid().history_size();
        let post_line = self.inner.term.grid().cursor.point.line.0;
        if post_hist != pre_hist || post_line == pre_line {
            self.inner.mark_full_damage();
        } else {
            self.inner.bump_visual();
        }
    }

    fn print_codepoint(&mut self, c: char) {
        self.inner.preceding_char = Some(c);

        // Mode 2027 retrospective grapheme clustering onto the previously printed cell.
        if self.inner.grapheme_cluster_mode && (c as u32) >= 0x80 {
            if let Some((line, col)) = self.inner.last_printed {
                let topmost = self.inner.term.topmost_line();
                let bottommost = self.inner.term.bottommost_line();
                let grid_cols = self.inner.term.columns();
                if line >= topmost && line <= bottommost && col.0 < grid_cols {
                    let continuation = {
                        let cell = &self.inner.term.grid()[line][col];
                        let current_wide = cell.flags.contains(Flags::WIDE_CHAR);
                        let zw = cell.zerowidth().unwrap_or(&[]);
                        check_grapheme_continuation(cell.c, zw, current_wide, c)
                    };
                    if let Some((new_width, should_store)) = continuation {
                        self.apply_grapheme_continuation(line, col, c, new_width, should_store);
                        return;
                    }
                }
            }
        }

        let logical_cols = usize::from(self.inner.logical_cols);
        let cp_width = unicode_codepoint_width(c as u32);
        match cp_width {
            0 => {
                if let Some((line, col)) = self.inner.last_printed {
                    let topmost = self.inner.term.topmost_line();
                    let bottommost = self.inner.term.bottommost_line();
                    if line >= topmost && line <= bottommost && col.0 < self.inner.term.columns() {
                        self.inner.term.grid_mut()[line][col].push_zerowidth(c);
                        if line.0 >= 0 {
                            self.inner.mark_row_dirty(line.0);
                        } else {
                            self.inner.mark_full_damage();
                        }
                    }
                }
            }
            1 => {
                // Handle single-column Logical_Cols == 1 edge case cleanly.
                if logical_cols == 1 {
                    self.print_narrow_col1(c);
                    return;
                }
                let pre_hist = self.inner.term.grid().history_size();
                let pre_line = self.inner.term.grid().cursor.point.line.0;
                let pre_wrap = self.inner.term.grid().cursor.input_needs_wrap;
                let wrap_enabled = self.inner.term.mode().contains(TermMode::LINE_WRAP);

                if UnicodeWidthChar::width(c) == Some(1) {
                    Handler::input(&mut self.inner.term, c);
                } else {
                    Handler::input(&mut self.inner.term, ' ');
                    let (wline, wcol) = self.written_narrow_pos();
                    self.inner.term.grid_mut()[wline][wcol].c = c;
                }
                let (wline, wcol) = self.written_narrow_pos();
                self.record_printed_cell(wline, wcol);
                if self.inner.term.grid().history_size() != pre_hist
                    || (pre_wrap && wrap_enabled && wline.0 <= pre_line)
                {
                    self.inner.mark_full_damage();
                } else {
                    self.inner.mark_row_dirty(wline.0);
                }
            }
            _ => {
                if logical_cols == 1 {
                    self.print_narrow_col1(c);
                    return;
                }
                if !self.inner.term.mode().contains(TermMode::LINE_WRAP)
                    && !self.inner.term.grid().cursor.input_needs_wrap
                    && self.inner.term.grid().cursor.point.column.0 + 1 >= logical_cols
                {
                    self.inner.term.grid_mut().cursor.input_needs_wrap = true;
                    self.inner.last_printed = None;
                    self.inner.bump_visual();
                    return;
                }
                let pre_hist = self.inner.term.grid().history_size();
                let pre_line = self.inner.term.grid().cursor.point.line.0;
                let pre_col = self.inner.term.grid().cursor.point.column.0;
                let pre_wrap = self.inner.term.grid().cursor.input_needs_wrap;
                let wrap_enabled = self.inner.term.mode().contains(TermMode::LINE_WRAP);

                if UnicodeWidthChar::width(c) == Some(2) {
                    Handler::input(&mut self.inner.term, c);
                } else {
                    Handler::input(&mut self.inner.term, '好');
                    let (wline, wcol) = self.written_wide_pos();
                    self.inner.term.grid_mut()[wline][wcol].c = c;
                }
                let (wline, wcol) = self.written_wide_pos();
                self.record_printed_cell(wline, wcol);
                let wrapped = (pre_wrap || pre_col + 1 >= logical_cols) && wrap_enabled;
                if self.inner.term.grid().history_size() != pre_hist
                    || (wrapped && wline.0 <= pre_line)
                {
                    self.inner.mark_full_damage();
                } else {
                    if !pre_wrap && pre_col + 1 >= logical_cols && wrap_enabled {
                        self.inner.mark_row_dirty(pre_line);
                    }
                    self.inner.mark_row_dirty(wline.0);
                }
            }
        }
    }

    fn print_narrow_col1(&mut self, c: char) {
        if self.inner.term.grid().cursor.input_needs_wrap
            && self.inner.term.mode().contains(TermMode::LINE_WRAP)
        {
            let cur_line = self.inner.term.grid().cursor.point.line;
            self.inner.term.grid_mut()[cur_line][Column(0)]
                .flags
                .insert(Flags::WRAPLINE);
            self.call_linefeed();
            Handler::carriage_return(&mut self.inner.term);
        }
        let line = self.inner.term.grid().cursor.point.line;
        Handler::input(&mut self.inner.term, ' ');
        self.inner.term.grid_mut()[line][Column(0)].c = c;
        self.inner.term.grid_mut().cursor.point.column = Column(0);
        self.inner.term.grid_mut().cursor.input_needs_wrap = true;
        self.record_printed_cell(line, Column(0));
        self.inner.mark_row_dirty(line.0);
    }

    #[inline]
    fn written_narrow_pos(&self) -> (Line, Column) {
        let cursor = &self.inner.term.grid().cursor;
        let col = if cursor.input_needs_wrap {
            cursor.point.column
        } else {
            Column(cursor.point.column.0.saturating_sub(1))
        };
        (cursor.point.line, col)
    }

    #[inline]
    fn written_wide_pos(&self) -> (Line, Column) {
        let cursor = &self.inner.term.grid().cursor;
        let col = if cursor.input_needs_wrap {
            Column(cursor.point.column.0.saturating_sub(1))
        } else {
            Column(cursor.point.column.0.saturating_sub(2))
        };
        (cursor.point.line, col)
    }

    #[inline]
    fn record_printed_cell(&mut self, line: Line, col: Column) {
        self.inner.term.grid_mut()[line][col]
            .flags
            .insert(FLAG_HAS_TEXT);
        self.inner.last_printed = Some((line, col));
        if self.inner.blink_active {
            self.inner.active_blink_cells_mut().insert((line.0, col.0));
        } else if !self.inner.active_blink_cells().is_empty() {
            self.inner.active_blink_cells_mut().remove(&(line.0, col.0));
        }
        if self.inner.overline_active {
            self.inner
                .active_overline_cells_mut()
                .insert((line.0, col.0));
        } else if !self.inner.active_overline_cells().is_empty() {
            self.inner
                .active_overline_cells_mut()
                .remove(&(line.0, col.0));
        }
    }

    fn apply_grapheme_continuation(
        &mut self,
        line: Line,
        col: Column,
        c: char,
        new_width: u8,
        should_store: bool,
    ) {
        self.inner.mark_full_damage();
        let logical_cols = usize::from(self.inner.logical_cols);
        let current_wide = self.inner.term.grid()[line][col]
            .flags
            .contains(Flags::WIDE_CHAR);

        if should_store {
            self.inner.term.grid_mut()[line][col].push_zerowidth(c);
        }

        if !current_wide && new_width == 2 && logical_cols >= 2 {
            if col.0 + 1 < logical_cols {
                let template_fg = self.inner.term.grid()[line][col].fg;
                let template_bg = self.inner.term.grid()[line][col].bg;
                let template_flags = self.inner.term.grid()[line][col].flags
                    & !(Flags::WIDE_CHAR
                        | Flags::WRAPLINE
                        | Flags::LEADING_WIDE_CHAR_SPACER
                        | FLAG_HAS_TEXT);
                let hyperlink = self.inner.term.grid()[line][col].hyperlink();
                self.inner.term.grid_mut()[line][col]
                    .flags
                    .insert(Flags::WIDE_CHAR);
                let spacer = &mut self.inner.term.grid_mut()[line][Column(col.0 + 1)];
                *spacer = Cell::default();
                spacer.c = ' ';
                spacer.fg = template_fg;
                spacer.bg = template_bg;
                spacer.flags = template_flags | Flags::WIDE_CHAR_SPACER;
                spacer.set_hyperlink(hyperlink);

                let cursor = &mut self.inner.term.grid_mut().cursor;
                if cursor.point.line == line {
                    if col.0 + 2 < logical_cols {
                        cursor.point.column = Column(col.0 + 2);
                        cursor.input_needs_wrap = false;
                    } else {
                        cursor.point.column = Column(logical_cols - 1);
                        cursor.input_needs_wrap = true;
                    }
                }
            } else if self.inner.term.mode().contains(TermMode::LINE_WRAP) {
                // Cluster at the rightmost column widened from 1 to 2: wrap entire cluster to next line.
                let mut moved = self.inner.term.grid()[line][col].clone();
                moved
                    .flags
                    .remove(Flags::WRAPLINE | Flags::LEADING_WIDE_CHAR_SPACER);
                moved.flags.insert(Flags::WIDE_CHAR);

                let placeholder = &mut self.inner.term.grid_mut()[line][col];
                let fg = placeholder.fg;
                let bg = placeholder.bg;
                *placeholder = Cell::default();
                placeholder.c = ' ';
                placeholder.fg = fg;
                placeholder.bg = bg;
                placeholder.flags = Flags::LEADING_WIDE_CHAR_SPACER | Flags::WRAPLINE;

                self.inner.term.grid_mut().cursor.point =
                    alacritty_terminal::index::Point::new(line, Column(0));
                self.inner.term.grid_mut().cursor.input_needs_wrap = false;
                Handler::linefeed(&mut self.inner.term);
                let new_line = self.inner.term.grid().cursor.point.line;

                let spacer_fg = moved.fg;
                let spacer_bg = moved.bg;
                let spacer_flags = moved.flags & !(Flags::WIDE_CHAR | FLAG_HAS_TEXT);
                let spacer_link = moved.hyperlink();
                self.inner.term.grid_mut()[new_line][Column(0)] = moved;
                let spacer = &mut self.inner.term.grid_mut()[new_line][Column(1)];
                *spacer = Cell::default();
                spacer.c = ' ';
                spacer.fg = spacer_fg;
                spacer.bg = spacer_bg;
                spacer.flags = spacer_flags | Flags::WIDE_CHAR_SPACER;
                spacer.set_hyperlink(spacer_link);

                let cursor = &mut self.inner.term.grid_mut().cursor;
                if logical_cols > 2 {
                    cursor.point = alacritty_terminal::index::Point::new(new_line, Column(2));
                    cursor.input_needs_wrap = false;
                } else {
                    cursor.point = alacritty_terminal::index::Point::new(new_line, Column(1));
                    cursor.input_needs_wrap = true;
                }
                self.inner.last_printed = Some((new_line, Column(0)));
            }
        } else if current_wide && new_width == 1 {
            self.inner.term.grid_mut()[line][col]
                .flags
                .remove(Flags::WIDE_CHAR);
            if col.0 + 1 < self.inner.term.columns() {
                let next_cell = &mut self.inner.term.grid_mut()[line][Column(col.0 + 1)];
                if next_cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    *next_cell = Cell::default();
                }
            }
            let cursor = &mut self.inner.term.grid_mut().cursor;
            if cursor.point.line == line {
                if col.0 + 1 < logical_cols {
                    cursor.point.column = Column(col.0 + 1);
                    cursor.input_needs_wrap = false;
                } else {
                    cursor.point.column = Column(logical_cols.saturating_sub(1));
                    cursor.input_needs_wrap = true;
                }
            }
        }
        self.inner.mark_full_damage();
    }

    fn handle_sgr(&mut self, params_iter: &mut ParamsIter<'_>) {
        while let Some(param) = params_iter.next() {
            match param {
                [0] | [] => {
                    Handler::terminal_attribute(&mut self.inner.term, Attr::Reset);
                    self.inner.blink_active = false;
                    self.inner.overline_active = false;
                }
                [1] => Handler::terminal_attribute(&mut self.inner.term, Attr::Bold),
                [2] => Handler::terminal_attribute(&mut self.inner.term, Attr::Dim),
                [3] => Handler::terminal_attribute(&mut self.inner.term, Attr::Italic),
                [4, 0] => Handler::terminal_attribute(&mut self.inner.term, Attr::CancelUnderline),
                [4, 2] => Handler::terminal_attribute(&mut self.inner.term, Attr::DoubleUnderline),
                [4, 3] => Handler::terminal_attribute(&mut self.inner.term, Attr::Undercurl),
                [4, 4] => Handler::terminal_attribute(&mut self.inner.term, Attr::DottedUnderline),
                [4, 5] => Handler::terminal_attribute(&mut self.inner.term, Attr::DashedUnderline),
                [4, ..] => Handler::terminal_attribute(&mut self.inner.term, Attr::Underline),
                [5] | [6] => {
                    self.inner.blink_active = true;
                }
                [7] => Handler::terminal_attribute(&mut self.inner.term, Attr::Reverse),
                [8] => Handler::terminal_attribute(&mut self.inner.term, Attr::Hidden),
                [9] => Handler::terminal_attribute(&mut self.inner.term, Attr::Strike),
                [21] => Handler::terminal_attribute(&mut self.inner.term, Attr::DoubleUnderline),
                [22] => Handler::terminal_attribute(&mut self.inner.term, Attr::CancelBoldDim),
                [23] => Handler::terminal_attribute(&mut self.inner.term, Attr::CancelItalic),
                [24] => Handler::terminal_attribute(&mut self.inner.term, Attr::CancelUnderline),
                [25] => {
                    self.inner.blink_active = false;
                }
                [27] => Handler::terminal_attribute(&mut self.inner.term, Attr::CancelReverse),
                [28] => Handler::terminal_attribute(&mut self.inner.term, Attr::CancelHidden),
                [29] => Handler::terminal_attribute(&mut self.inner.term, Attr::CancelStrike),
                [53] => {
                    self.inner.overline_active = true;
                }
                [55] => {
                    self.inner.overline_active = false;
                }
                [n @ 30..=37] => {
                    if let Some(c) = ansi_named_fg(*n - 30, false) {
                        Handler::terminal_attribute(&mut self.inner.term, Attr::Foreground(c));
                    }
                }
                [38] => {
                    let mut mapped = params_iter.map(|p| p[0]);
                    if let Some(c) = parse_sgr_color(&mut mapped) {
                        Handler::terminal_attribute(&mut self.inner.term, Attr::Foreground(c));
                    }
                }
                [38, rest @ ..] => {
                    if let Some(c) = parse_colon_sgr_color(rest) {
                        Handler::terminal_attribute(&mut self.inner.term, Attr::Foreground(c));
                    }
                }
                [39] => Handler::terminal_attribute(
                    &mut self.inner.term,
                    Attr::Foreground(AnsiColor::Named(NamedColor::Foreground)),
                ),
                [n @ 40..=47] => {
                    if let Some(c) = ansi_named_fg(*n - 40, false) {
                        Handler::terminal_attribute(&mut self.inner.term, Attr::Background(c));
                    }
                }
                [48] => {
                    let mut mapped = params_iter.map(|p| p[0]);
                    if let Some(c) = parse_sgr_color(&mut mapped) {
                        Handler::terminal_attribute(&mut self.inner.term, Attr::Background(c));
                    }
                }
                [48, rest @ ..] => {
                    if let Some(c) = parse_colon_sgr_color(rest) {
                        Handler::terminal_attribute(&mut self.inner.term, Attr::Background(c));
                    }
                }
                [49] => Handler::terminal_attribute(
                    &mut self.inner.term,
                    Attr::Background(AnsiColor::Named(NamedColor::Background)),
                ),
                [58] => {
                    let mut mapped = params_iter.map(|p| p[0]);
                    if let Some(c) = parse_sgr_color(&mut mapped) {
                        Handler::terminal_attribute(
                            &mut self.inner.term,
                            Attr::UnderlineColor(Some(c)),
                        );
                    }
                }
                [58, rest @ ..] => {
                    if let Some(c) = parse_colon_sgr_color(rest) {
                        Handler::terminal_attribute(
                            &mut self.inner.term,
                            Attr::UnderlineColor(Some(c)),
                        );
                    }
                }
                [59] => {
                    Handler::terminal_attribute(&mut self.inner.term, Attr::UnderlineColor(None))
                }
                [n @ 90..=97] => {
                    if let Some(c) = ansi_named_fg(*n - 90, true) {
                        Handler::terminal_attribute(&mut self.inner.term, Attr::Foreground(c));
                    }
                }
                [n @ 100..=107] => {
                    if let Some(c) = ansi_named_fg(*n - 100, true) {
                        Handler::terminal_attribute(&mut self.inner.term, Attr::Background(c));
                    }
                }
                _ => {}
            }
        }
    }
}

impl Perform for VtPerformer<'_> {
    #[inline]
    fn print(&mut self, c: char) {
        self.print_codepoint(c);
    }

    #[inline]
    fn execute(&mut self, byte: u8) {
        match byte {
            0x07 => {
                self.inner.bell_count = self.inner.bell_count.saturating_add(1);
            }
            0x08 => {
                self.inner.last_printed = None;
                self.inner.bump_visual();
                Handler::backspace(&mut self.inner.term);
            }
            0x09 => {
                self.inner.last_printed = None;
                let line = self.inner.term.grid().cursor.point.line.0;
                Handler::put_tab(&mut self.inner.term, 1);
                self.inner.mark_row_dirty(line);
            }
            0x0a..=0x0c => {
                self.inner.last_printed = None;
                self.call_linefeed();
            }
            0x0d => {
                self.inner.last_printed = None;
                self.inner.bump_visual();
                Handler::carriage_return(&mut self.inner.term);
            }
            0x0e => {
                self.inner.last_printed = None;
                Handler::set_active_charset(&mut self.inner.term, CharsetIndex::G1);
            }
            0x0f => {
                self.inner.last_printed = None;
                Handler::set_active_charset(&mut self.inner.term, CharsetIndex::G0);
            }
            _ => {}
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &Params,
        intermediates: &[u8],
        has_ignored_intermediates: bool,
        action: char,
    ) {
        self.inner.last_printed = None;
        if has_ignored_intermediates || intermediates.len() > 2 {
            return;
        }

        let mut params_iter = params.iter();
        let mut next_param_or = |default: u16| match params_iter.next() {
            Some(&[param, ..]) if param != 0 => param,
            _ => default,
        };

        match (action, intermediates) {
            ('@', []) => {
                let line = self.inner.term.grid().cursor.point.line.0;
                Handler::insert_blank(&mut self.inner.term, usize::from(next_param_or(1)));
                self.inner.mark_row_dirty(line);
            }
            ('A', []) => {
                self.inner.bump_visual();
                Handler::move_up(&mut self.inner.term, usize::from(next_param_or(1)));
            }
            ('B', []) | ('e', []) => {
                self.inner.bump_visual();
                Handler::move_down(&mut self.inner.term, usize::from(next_param_or(1)));
            }
            ('b', []) => {
                if let Some(c) = self.inner.preceding_char {
                    let count = next_param_or(1);
                    for _ in 0..count {
                        self.print_codepoint(c);
                    }
                }
            }
            ('C', []) | ('a', []) => {
                self.inner.bump_visual();
                Handler::move_forward(&mut self.inner.term, usize::from(next_param_or(1)));
            }
            ('c', []) => {
                if next_param_or(0) == 0 {
                    self.inner.flush_term_events();
                    self.inner.emit_pty(b"\x1b[?62;22c");
                }
            }
            ('c', [b'>']) => {
                if next_param_or(0) == 0 {
                    self.inner.flush_term_events();
                    self.inner.emit_pty(b"\x1b[>1;10;0c");
                }
            }
            ('D', []) => {
                self.inner.bump_visual();
                Handler::move_backward(&mut self.inner.term, usize::from(next_param_or(1)));
            }
            ('d', []) => {
                self.inner.bump_visual();
                Handler::goto_line(&mut self.inner.term, i32::from(next_param_or(1)) - 1);
            }
            ('E', []) => {
                self.inner.bump_visual();
                Handler::move_down_and_cr(&mut self.inner.term, usize::from(next_param_or(1)));
            }
            ('F', []) => {
                self.inner.bump_visual();
                Handler::move_up_and_cr(&mut self.inner.term, usize::from(next_param_or(1)));
            }
            ('G', []) | ('`', []) => {
                self.inner.bump_visual();
                let max_col = usize::from(self.inner.logical_cols.saturating_sub(1));
                let col = usize::from(next_param_or(1)).saturating_sub(1).min(max_col);
                Handler::goto_col(&mut self.inner.term, col);
            }
            ('W', [b'?']) if next_param_or(0) == 5 => {
                Handler::set_tabs(&mut self.inner.term, 8);
            }
            ('g', []) => match next_param_or(0) {
                0 => Handler::clear_tabs(&mut self.inner.term, TabulationClearMode::Current),
                3 => Handler::clear_tabs(&mut self.inner.term, TabulationClearMode::All),
                _ => {}
            },
            ('H', []) | ('f', []) => {
                self.inner.bump_visual();
                let y = i32::from(next_param_or(1)) - 1;
                let max_col = usize::from(self.inner.logical_cols.saturating_sub(1));
                let x = usize::from(next_param_or(1)).saturating_sub(1).min(max_col);
                Handler::goto(&mut self.inner.term, y, x);
            }
            ('h', []) => {
                for param in params_iter.map(|p| p[0]) {
                    let mode = match param {
                        4 => AnsiMode::Named(NamedMode::Insert),
                        20 => AnsiMode::Named(NamedMode::LineFeedNewLine),
                        _ => AnsiMode::Unknown(param),
                    };
                    Handler::set_mode(&mut self.inner.term, mode);
                }
                self.inner.bump_visual();
            }
            ('h', [b'?']) => {
                let modes: Vec<u16> = params_iter.map(|p| p[0]).collect();
                for mode in modes {
                    self.inner.set_private_mode_num(mode, true, true);
                }
            }
            ('I', []) => {
                self.inner.bump_visual();
                Handler::move_forward_tabs(&mut self.inner.term, next_param_or(1));
            }
            ('J', [] | [b'?']) => {
                let mode = match next_param_or(0) {
                    0 => ClearMode::Below,
                    1 => ClearMode::Above,
                    2 => ClearMode::All,
                    3 => ClearMode::Saved,
                    _ => return,
                };
                match mode {
                    ClearMode::All => {
                        self.inner.active_blink_cells_mut().clear();
                        self.inner.active_overline_cells_mut().clear();
                        self.inner.term.grid_mut().reset_region(..);
                        self.inner.term.grid_mut().cursor.input_needs_wrap = false;
                        self.inner.term.selection = None;
                        self.inner.mark_full_damage();
                    }
                    ClearMode::Saved => {
                        Handler::clear_screen(&mut self.inner.term, ClearMode::Saved);
                        if self.inner.term.grid().display_offset() != 0 {
                            self.inner.mark_full_damage();
                        }
                    }
                    other => {
                        Handler::clear_screen(&mut self.inner.term, other);
                        self.inner.mark_full_damage();
                    }
                }
            }
            ('K', [] | [b'?']) => {
                let mode = match next_param_or(0) {
                    0 => LineClearMode::Right,
                    1 => LineClearMode::Left,
                    2 => LineClearMode::All,
                    _ => return,
                };
                let line = self.inner.term.grid().cursor.point.line.0;
                Handler::clear_line(&mut self.inner.term, mode);
                self.inner.mark_row_dirty(line);
            }
            ('k', [b' ']) => {
                let char_path = match next_param_or(0) {
                    0 => ScpCharPath::Default,
                    1 => ScpCharPath::LTR,
                    2 => ScpCharPath::RTL,
                    _ => return,
                };
                let update_mode = match next_param_or(0) {
                    0 => ScpUpdateMode::ImplementationDependant,
                    1 => ScpUpdateMode::DataToPresentation,
                    2 => ScpUpdateMode::PresentationToData,
                    _ => return,
                };
                Handler::set_scp(&mut self.inner.term, char_path, update_mode);
            }
            ('L', []) => {
                self.inner.mark_full_damage();
                Handler::insert_blank_lines(&mut self.inner.term, usize::from(next_param_or(1)));
            }
            ('l', []) => {
                for param in params_iter.map(|p| p[0]) {
                    let mode = match param {
                        4 => AnsiMode::Named(NamedMode::Insert),
                        20 => AnsiMode::Named(NamedMode::LineFeedNewLine),
                        _ => AnsiMode::Unknown(param),
                    };
                    Handler::unset_mode(&mut self.inner.term, mode);
                }
                self.inner.bump_visual();
            }
            ('l', [b'?']) => {
                let modes: Vec<u16> = params_iter.map(|p| p[0]).collect();
                for mode in modes {
                    self.inner.set_private_mode_num(mode, false, true);
                }
            }
            ('M', []) => {
                self.inner.mark_full_damage();
                Handler::delete_lines(&mut self.inner.term, usize::from(next_param_or(1)));
            }
            ('m', []) => {
                if params.is_empty() {
                    Handler::terminal_attribute(&mut self.inner.term, Attr::Reset);
                    self.inner.blink_active = false;
                    self.inner.overline_active = false;
                } else {
                    self.handle_sgr(&mut params_iter);
                }
            }
            ('m', [b'>']) => {
                // xterm modifyOtherKeys: `CSI > m`, `CSI > 4 m`, `CSI > 4 ; <0|1|2> m`
                let raw: Vec<u16> = params.iter().map(|p| p[0]).collect();
                match raw.as_slice() {
                    [] | [0] | [4] | [4, 0] => {
                        self.inner.modify_other_keys_state = 0;
                    }
                    [4, 1] => {
                        self.inner.modify_other_keys_state = 1;
                    }
                    [4, 2] => {
                        self.inner.modify_other_keys_state = 2;
                    }
                    _ => {}
                }
            }
            ('m', [b'?']) => {
                if params_iter.next() == Some(&[4]) {
                    self.inner.flush_term_events();
                    let reply = format!("\x1b[>4;{}m", self.inner.modify_other_keys_state);
                    self.inner.emit_pty(reply.as_bytes());
                }
            }
            ('n', []) => {
                let arg = usize::from(next_param_or(0));
                Handler::device_status(&mut self.inner.term, arg);
            }
            ('n', [b'>']) => {
                // xterm reset modifyOtherKeys: `CSI > 4 n` / `CSI > n`
                let raw: Vec<u16> = params.iter().map(|p| p[0]).collect();
                if matches!(raw.as_slice(), [] | [0] | [4]) {
                    self.inner.modify_other_keys_state = 0;
                }
            }
            ('n', [b'?']) => {
                let arg = next_param_or(0);
                match arg {
                    6 => {
                        self.inner.flush_term_events();
                        let point = self.inner.term.grid().cursor.point;
                        let row = point.line.0.max(0) + 1;
                        let col = point.column.0 + 1;
                        let reply = format!("\x1b[?{row};{col}R");
                        self.inner.emit_pty(reply.as_bytes());
                    }
                    996 => {
                        self.inner.flush_term_events();
                        match self.inner.color_scheme {
                            Some(ColorScheme::Dark) => self.inner.emit_pty(b"\x1b[?997;1n"),
                            Some(ColorScheme::Light) => self.inner.emit_pty(b"\x1b[?997;2n"),
                            None => {}
                        }
                    }
                    _ => {}
                }
            }
            ('P', []) => {
                let line = self.inner.term.grid().cursor.point.line.0;
                Handler::delete_chars(&mut self.inner.term, usize::from(next_param_or(1)));
                self.inner.mark_row_dirty(line);
            }
            ('p', [b'$']) => {
                let mode = next_param_or(0);
                let pm = match self.inner.get_mode_internal(mode, true) {
                    Some(true) => 1,
                    Some(false) => 2,
                    None => 0,
                };
                self.inner.flush_term_events();
                let reply = format!("\x1b[{mode};{pm}$y");
                self.inner.emit_pty(reply.as_bytes());
            }
            ('p', [b'?', b'$']) => {
                let mode = next_param_or(0);
                let pm = match self.inner.get_mode_internal(mode, false) {
                    Some(true) => 1,
                    Some(false) => 2,
                    None => 0,
                };
                self.inner.flush_term_events();
                let reply = format!("\x1b[?{mode};{pm}$y");
                self.inner.emit_pty(reply.as_bytes());
            }
            ('p', [b'!']) => {
                // DECSTR soft reset
                self.inner.cursor_hollow = false;
                self.inner.decscusr_blinking = None;
                self.inner.blink_active = false;
                self.inner.overline_active = false;
                Handler::terminal_attribute(&mut self.inner.term, Attr::Reset);
                self.inner.set_private_mode_num(1, false, true);
                self.inner.set_private_mode_num(6, false, true);
                self.inner.set_private_mode_num(7, true, true);
                self.inner.set_private_mode_num(25, true, true);
                self.inner.set_private_mode_num(66, false, true);
                Handler::set_scrolling_region(&mut self.inner.term, 1, None);
                self.inner.bump_visual();
            }
            ('q', [b' ']) => {
                // DECSCUSR
                let id = next_param_or(0);
                self.inner.cursor_hollow = false;
                let style = match id {
                    0 => {
                        self.inner.decscusr_blinking = None;
                        None
                    }
                    1 | 2 => {
                        let blinking = id == 1;
                        self.inner.decscusr_blinking = Some(blinking);
                        Some(CursorStyle {
                            shape: CursorShape::Block,
                            blinking,
                        })
                    }
                    3 | 4 => {
                        let blinking = id == 3;
                        self.inner.decscusr_blinking = Some(blinking);
                        Some(CursorStyle {
                            shape: CursorShape::Underline,
                            blinking,
                        })
                    }
                    5 | 6 => {
                        let blinking = id == 5;
                        self.inner.decscusr_blinking = Some(blinking);
                        Some(CursorStyle {
                            shape: CursorShape::Beam,
                            blinking,
                        })
                    }
                    _ => return,
                };
                Handler::set_cursor_style(&mut self.inner.term, style);
                self.inner.bump_visual();
            }
            ('r', []) => {
                let top = usize::from(next_param_or(1));
                let bottom = params_iter
                    .next()
                    .map(|param| usize::from(param[0]))
                    .filter(|&param| param != 0);
                Handler::set_scrolling_region(&mut self.inner.term, top, bottom);
                self.inner.bump_visual();
            }
            ('S', []) => {
                self.inner.mark_full_damage();
                Handler::scroll_up(&mut self.inner.term, usize::from(next_param_or(1)));
            }
            ('s', []) => {
                Handler::save_cursor_position(&mut self.inner.term);
            }
            ('T', []) => {
                self.inner.mark_full_damage();
                Handler::scroll_down(&mut self.inner.term, usize::from(next_param_or(1)));
            }
            ('t', []) => {
                let op = next_param_or(0);
                match op {
                    14 => {
                        if self.inner.cell_width_px > 0 && self.inner.cell_height_px > 0 {
                            self.inner.flush_term_events();
                            let reply = format!(
                                "\x1b[4;{};{}t",
                                self.inner.height_px(),
                                self.inner.width_px()
                            );
                            self.inner.emit_pty(reply.as_bytes());
                        }
                    }
                    16 => {
                        if self.inner.cell_width_px > 0 && self.inner.cell_height_px > 0 {
                            self.inner.flush_term_events();
                            let reply = format!(
                                "\x1b[6;{};{}t",
                                self.inner.cell_height_px, self.inner.cell_width_px
                            );
                            self.inner.emit_pty(reply.as_bytes());
                        }
                    }
                    18 => {
                        if self.inner.cell_width_px > 0 && self.inner.cell_height_px > 0 {
                            self.inner.flush_term_events();
                            let reply = format!(
                                "\x1b[8;{};{}t",
                                self.inner.logical_rows, self.inner.logical_cols
                            );
                            self.inner.emit_pty(reply.as_bytes());
                        }
                    }
                    22 => Handler::push_title(&mut self.inner.term),
                    23 => Handler::pop_title(&mut self.inner.term),
                    _ => {}
                }
            }
            ('u', [b'?']) => {
                self.inner.flush_term_events();
                let flags = self.inner.current_kitty_flags();
                let reply = format!("\x1b[?{flags}u");
                self.inner.emit_pty(reply.as_bytes());
            }
            ('u', [b'>']) => {
                let raw = (next_param_or(0) as u8) & ffi::GHOSTTY_KITTY_KEY_ALL;
                let stack = self.inner.active_kitty_stack_mut();
                if stack.len() >= KITTY_STACK_CAP {
                    stack.remove(0);
                }
                stack.push(raw);
            }
            ('u', [b'<']) => {
                let count = usize::from(next_param_or(1));
                let stack = self.inner.active_kitty_stack_mut();
                let new_len = stack.len().saturating_sub(count);
                stack.truncate(new_len);
            }
            ('u', [b'=']) => {
                let flags = (next_param_or(0) as u8) & ffi::GHOSTTY_KITTY_KEY_ALL;
                let mode = next_param_or(1);
                let stack = self.inner.active_kitty_stack_mut();
                if stack.is_empty() {
                    stack.push(0);
                }
                if let Some(top) = stack.last_mut() {
                    match mode {
                        2 => *top |= flags,
                        3 => *top &= !flags,
                        _ => *top = flags,
                    }
                }
            }
            ('u', []) => {
                self.inner.bump_visual();
                Handler::restore_cursor_position(&mut self.inner.term);
            }
            ('X', []) => {
                let line = self.inner.term.grid().cursor.point.line.0;
                Handler::erase_chars(&mut self.inner.term, usize::from(next_param_or(1)));
                self.inner.mark_row_dirty(line);
            }
            ('Z', []) => {
                self.inner.bump_visual();
                Handler::move_backward_tabs(&mut self.inner.term, next_param_or(1));
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        self.inner.last_printed = None;
        let charset_index = |ints: &[u8]| -> Option<CharsetIndex> {
            match ints {
                [b'('] => Some(CharsetIndex::G0),
                [b')'] => Some(CharsetIndex::G1),
                [b'*'] => Some(CharsetIndex::G2),
                [b'+'] => Some(CharsetIndex::G3),
                _ => None,
            }
        };

        match (byte, intermediates) {
            (b'B', ints) => {
                if let Some(idx) = charset_index(ints) {
                    Handler::configure_charset(&mut self.inner.term, idx, StandardCharset::Ascii);
                }
            }
            (b'0', ints) => {
                if let Some(idx) = charset_index(ints) {
                    Handler::configure_charset(
                        &mut self.inner.term,
                        idx,
                        StandardCharset::SpecialCharacterAndLineDrawing,
                    );
                }
            }
            (b'D', []) => {
                self.call_linefeed();
            }
            (b'E', []) => {
                self.call_linefeed();
                Handler::carriage_return(&mut self.inner.term);
            }
            (b'H', []) => {
                Handler::set_horizontal_tabstop(&mut self.inner.term);
            }
            (b'M', []) => {
                let pre_line = self.inner.term.grid().cursor.point.line.0;
                Handler::reverse_index(&mut self.inner.term);
                if self.inner.term.grid().cursor.point.line.0 == pre_line {
                    self.inner.mark_full_damage();
                } else {
                    self.inner.bump_visual();
                }
            }
            (b'Z', []) => {
                self.inner.flush_term_events();
                self.inner.emit_pty(b"\x1b[?62;22c");
            }
            (b'c', []) => {
                self.inner.reset_all_state();
            }
            (b'7', []) => {
                Handler::save_cursor_position(&mut self.inner.term);
            }
            (b'8', [b'#']) => {
                self.inner.mark_full_damage();
                Handler::decaln(&mut self.inner.term);
            }
            (b'8', []) => {
                self.inner.bump_visual();
                Handler::restore_cursor_position(&mut self.inner.term);
            }
            (b'=', []) => {
                Handler::set_keypad_application_mode(&mut self.inner.term);
            }
            (b'>', []) => {
                Handler::unset_keypad_application_mode(&mut self.inner.term);
            }
            _ => {}
        }
    }
}

fn ansi_named_fg(offset: u16, bright: bool) -> Option<AnsiColor> {
    let named = match (offset, bright) {
        (0, false) => NamedColor::Black,
        (1, false) => NamedColor::Red,
        (2, false) => NamedColor::Green,
        (3, false) => NamedColor::Yellow,
        (4, false) => NamedColor::Blue,
        (5, false) => NamedColor::Magenta,
        (6, false) => NamedColor::Cyan,
        (7, false) => NamedColor::White,
        (0, true) => NamedColor::BrightBlack,
        (1, true) => NamedColor::BrightRed,
        (2, true) => NamedColor::BrightGreen,
        (3, true) => NamedColor::BrightYellow,
        (4, true) => NamedColor::BrightBlue,
        (5, true) => NamedColor::BrightMagenta,
        (6, true) => NamedColor::BrightCyan,
        (7, true) => NamedColor::BrightWhite,
        _ => return None,
    };
    Some(AnsiColor::Named(named))
}

fn parse_colon_sgr_color(subparams: &[u16]) -> Option<AnsiColor> {
    if subparams.is_empty() {
        return None;
    }
    match subparams[0] {
        2 => {
            let rgb_start = if subparams.len() >= 5 { 2 } else { 1 };
            let r = u8::try_from(*subparams.get(rgb_start)?).ok()?;
            let g = u8::try_from(*subparams.get(rgb_start + 1)?).ok()?;
            let b = u8::try_from(*subparams.get(rgb_start + 2)?).ok()?;
            Some(AnsiColor::Spec(AnsiRgb { r, g, b }))
        }
        5 => {
            let idx = u8::try_from(*subparams.get(1)?).ok()?;
            Some(AnsiColor::Indexed(idx))
        }
        _ => None,
    }
}

fn parse_sgr_color(params: &mut dyn Iterator<Item = u16>) -> Option<AnsiColor> {
    match params.next()? {
        2 => Some(AnsiColor::Spec(AnsiRgb {
            r: u8::try_from(params.next()?).ok()?,
            g: u8::try_from(params.next()?).ok()?,
            b: u8::try_from(params.next()?).ok()?,
        })),
        5 => Some(AnsiColor::Indexed(u8::try_from(params.next()?).ok()?)),
        _ => None,
    }
}

fn parse_u16_ascii(bytes: &[u8]) -> Option<u16> {
    if bytes.is_empty() {
        return None;
    }
    let mut val: u16 = 0;
    for &b in bytes {
        if !b.is_ascii_digit() {
            return None;
        }
        val = val.checked_mul(10)?.checked_add(u16::from(b - b'0'))?;
    }
    Some(val)
}

fn parse_x_color(spec: &[u8]) -> Option<RgbColor> {
    if let Some(hex) = spec.strip_prefix(b"#") {
        return match hex.len() {
            3 => Some(RgbColor {
                r: scale_hex_channel(hex, 1)?,
                g: scale_hex_channel(&hex[1..], 1)?,
                b: scale_hex_channel(&hex[2..], 1)?,
            }),
            6 => Some(RgbColor {
                r: scale_hex_channel(hex, 2)?,
                g: scale_hex_channel(&hex[2..], 2)?,
                b: scale_hex_channel(&hex[4..], 2)?,
            }),
            9 => Some(RgbColor {
                r: scale_hex_channel(hex, 3)?,
                g: scale_hex_channel(&hex[3..], 3)?,
                b: scale_hex_channel(&hex[6..], 3)?,
            }),
            12 => Some(RgbColor {
                r: scale_hex_channel(hex, 4)?,
                g: scale_hex_channel(&hex[4..], 4)?,
                b: scale_hex_channel(&hex[8..], 4)?,
            }),
            _ => None,
        };
    }
    if let Some(rest) = spec
        .strip_prefix(b"rgb:")
        .or_else(|| spec.strip_prefix(b"RGB:"))
    {
        let mut parts = rest.split(|&b| b == b'/');
        let r = parts.next()?;
        let g = parts.next()?;
        let b = parts.next()?;
        if parts.next().is_some()
            || !(1..=4).contains(&r.len())
            || !(1..=4).contains(&g.len())
            || !(1..=4).contains(&b.len())
        {
            return None;
        }
        return Some(RgbColor {
            r: scale_hex_channel(r, r.len())?,
            g: scale_hex_channel(g, g.len())?,
            b: scale_hex_channel(b, b.len())?,
        });
    }
    None
}

fn scale_hex_channel(slice: &[u8], len: usize) -> Option<u8> {
    let raw = slice.get(..len)?;
    let mut val: u32 = 0;
    for &b in raw {
        let nibble = match b {
            b'0'..=b'9' => u32::from(b - b'0'),
            b'a'..=b'f' => u32::from(b - b'a' + 10),
            b'A'..=b'F' => u32::from(b - b'A' + 10),
            _ => return None,
        };
        val = (val << 4) | nibble;
    }
    let max = (1u32 << (len * 4)) - 1;
    Some(((val * 255 + max / 2) / max) as u8)
}

fn write_osc_color_reply(out: &mut Vec<u8>, prefix: &str, c: RgbColor, terminator: &[u8]) {
    let r16 = u16::from(c.r) * 0x0101;
    let g16 = u16::from(c.g) * 0x0101;
    let b16 = u16::from(c.b) * 0x0101;
    let mut s = String::with_capacity(32);
    let _ = write!(s, "\x1b]{prefix};rgb:{r16:04x}/{g16:04x}/{b16:04x}");
    out.extend_from_slice(s.as_bytes());
    out.extend_from_slice(terminator);
}

fn decode_osc52_base64(input: &[u8]) -> Option<Vec<u8>> {
    let mut cleaned: Vec<u8> = input
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    if cleaned.is_empty() {
        return Some(Vec::new());
    }
    while !cleaned.len().is_multiple_of(4) {
        cleaned.push(b'=');
    }
    base64::engine::general_purpose::STANDARD
        .decode(&cleaned)
        .ok()
}

pub struct Terminal {
    pub(crate) inner: Mutex<TerminalInner>,
}

impl Terminal {
    pub fn new(cols: u16, rows: u16, max_scrollback: usize) -> Result<Self, Error> {
        Ok(Self {
            inner: Mutex::new(TerminalInner::new(cols, rows, max_scrollback)?),
        })
    }

    #[inline]
    pub(crate) fn lock_inner(&self) -> std::sync::MutexGuard<'_, TerminalInner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[inline]
    pub(crate) fn inner_mut(&mut self) -> &mut TerminalInner {
        self.inner
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn write(&mut self, bytes: &[u8]) {
        self.inner_mut().write(bytes);
    }

    pub fn compression_activity(&self) -> Result<u64, Error> {
        Ok(self.lock_inner().write_activity)
    }

    pub fn compress_incremental(&mut self) -> Result<TerminalCompressionResult, Error> {
        Ok(TerminalCompressionResult::Unsupported)
    }

    pub fn set_default_palette(&mut self, palette: &[RgbColor; 256]) -> Result<(), Error> {
        let inner = self.inner_mut();
        inner.default_palette = *palette;
        for (idx, &rgb) in palette.iter().enumerate() {
            if !inner.palette_overridden[idx] {
                inner.active_palette[idx] = rgb;
            }
        }
        inner.mark_full_damage();
        Ok(())
    }

    pub fn default_palette(&self) -> Result<[RgbColor; 256], Error> {
        Ok(self.lock_inner().default_palette)
    }

    pub fn resize(
        &mut self,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> Result<(), Error> {
        if cols == 0 || rows == 0 {
            return Err(Error(ffi::GhosttyResult_GHOSTTY_INVALID_VALUE));
        }
        let inner = self.inner_mut();
        let size_changed = inner.logical_cols != cols
            || inner.logical_rows != rows
            || inner.cell_width_px != cell_width_px
            || inner.cell_height_px != cell_height_px;

        let new_history = scrollback_lines_for_bytes(inner.max_scrollback, cols);
        let old_cols = inner.logical_cols;
        let old_rows = inner.logical_rows;

        if old_cols != cols || old_rows != rows {
            // Grow scrollback cap before shrinking columns so reflow preserves deep scrollback.
            if new_history > inner.term.grid().history_size() && !inner.is_alt_screen() {
                inner.term.grid_mut().update_history(new_history);
            }

            let history_before = inner.term.grid().history_size();
            let cursor_before_usize = inner.term.grid().cursor.point.line.0.max(0) as usize;
            let rows_usize = usize::from(rows);
            let grid_cols = usize::from(cols).max(alacritty_terminal::term::MIN_COLUMNS);

            #[cfg(windows)]
            let tracked_delta = {
                let history_size = history_before as i32;
                let cursor_line = inner.term.grid().cursor.point.line.0;
                let raw_delta = i32::from(rows) - i32::from(old_rows);
                let min_delta = (i32::from(rows) - cursor_line - 1).min(0);
                raw_delta.max(min_delta).min(history_size)
            };

            let dims = TermDimensions {
                cols: grid_cols,
                screen_lines: rows_usize,
            };
            inner.term.resize(dims);

            // When shrinking columns on the primary screen wraps content into additional physical
            // rows, consume untouched trailing blank rows strictly below the cursor before
            // scrolling upper viewport rows into scrollback history.
            #[allow(unused_variables)] // `reclaimed` is only read on Windows for tracked_grid_line.
            let reclaimed = if !inner.is_alt_screen() && cols < old_cols && rows_usize > 1 {
                let history_mid = if rows > old_rows {
                    history_before.saturating_sub(usize::from(rows - old_rows))
                } else {
                    let shrink_scroll = (cursor_before_usize + 1).saturating_sub(rows_usize);
                    history_before.saturating_add(shrink_scroll)
                };
                let pushed_by_wrap = inner.term.grid().history_size().saturating_sub(history_mid);
                let cursor_after = inner.term.grid().cursor.point.line.0.max(0) as usize;
                let mut trailing_blank = 0usize;
                if pushed_by_wrap > 0 && cursor_after + 1 < rows_usize {
                    for y in ((cursor_after + 1)..rows_usize).rev() {
                        let row = &inner.term.grid()[Line(y as i32)];
                        let is_blank = (0..grid_cols).all(|x| {
                            let cell = &row[Column(x)];
                            !cell.flags.contains(FLAG_HAS_TEXT)
                                && cell.hyperlink().is_none()
                                && cell.is_empty()
                        });
                        if is_blank {
                            trailing_blank += 1;
                        } else {
                            break;
                        }
                    }
                }
                let reclaim = pushed_by_wrap.min(trailing_blank);
                if reclaim > 0 {
                    inner
                        .term
                        .grid_mut()
                        .resize(true, rows_usize - reclaim, grid_cols);
                    inner.term.grid_mut().resize(true, rows_usize, grid_cols);
                }
                reclaim
            } else {
                0
            };

            #[cfg(windows)]
            if let Some((screen, line)) = &mut inner.tracked_grid_line {
                if *screen == inner.active_screen() {
                    *line += tracked_delta + (reclaimed as i32);
                } else {
                    inner.tracked_grid_line = None;
                }
            }

            if !inner.is_alt_screen() {
                inner.term.grid_mut().update_history(new_history);
            }
            inner.dirty_rows.resize(rows_usize, false);
            inner.dirty_rows.fill(false);
            inner.last_printed = None;
            inner.mark_full_damage();
        }

        inner.logical_cols = cols;
        inner.logical_rows = rows;
        inner.cell_width_px = cell_width_px;
        inner.cell_height_px = cell_height_px;

        if size_changed && inner.in_band_size_reports {
            inner.emit_in_band_size_report();
        }
        Ok(())
    }

    pub fn enable_kitty_graphics(&mut self) -> Result<(), Error> {
        Ok(())
    }

    pub fn set_kitty_source_forwarding(&mut self, _enabled: bool) -> Result<(), Error> {
        Ok(())
    }

    pub fn set_write_pty_callback<F>(&mut self, callback: F) -> Result<(), Error>
    where
        F: FnMut(&[u8]) + Send + 'static,
    {
        self.inner_mut().write_pty = Some(Box::new(callback));
        Ok(())
    }

    pub fn set_color_scheme(&mut self, color_scheme: Option<ColorScheme>) -> Option<ColorScheme> {
        mem::replace(&mut self.inner_mut().color_scheme, color_scheme)
    }

    pub fn take_bell_count(&mut self) -> u16 {
        mem::take(&mut self.inner_mut().bell_count)
    }

    pub fn take_pwd_changes(&mut self) -> Vec<Vec<u8>> {
        mem::take(&mut self.inner_mut().pwd_changes)
    }

    pub fn take_clipboard_writes(&mut self) -> Vec<Vec<u8>> {
        mem::take(&mut self.inner_mut().clipboard_writes)
    }

    pub fn mode_get(&self, mode: u16) -> Result<bool, Error> {
        let inner = self.lock_inner();
        inner
            .get_mode_internal(mode, false)
            .ok_or(Error(ffi::GhosttyResult_GHOSTTY_INVALID_VALUE))
    }

    pub fn mode_set(&mut self, mode: u16, value: bool) -> Result<(), Error> {
        let inner = self.inner_mut();
        if inner.get_mode_internal(mode, false).is_none() {
            return Err(Error(ffi::GhosttyResult_GHOSTTY_INVALID_VALUE));
        }
        inner.set_private_mode_num(mode, value, false);
        Ok(())
    }

    pub fn kitty_keyboard_flags(&self) -> Result<u8, Error> {
        Ok(self.lock_inner().current_kitty_flags())
    }

    pub fn mouse_tracking_enabled(&self) -> Result<bool, Error> {
        Ok(self.lock_inner().mouse_tracking_mode
            != ffi::GhosttyMouseTrackingMode_GHOSTTY_MOUSE_TRACKING_NONE)
    }

    pub fn modify_other_keys_enabled(&self) -> Result<bool, Error> {
        Ok(self.lock_inner().modify_other_keys_state == 2)
    }

    pub(crate) fn key_encoder_state(&self) -> KeyEncoderState {
        let inner = self.lock_inner();
        let tm = *inner.term.mode();
        KeyEncoderState {
            cursor_key_application: tm.contains(TermMode::APP_CURSOR),
            keypad_key_application: tm.contains(TermMode::APP_KEYPAD),
            backarrow_key_mode: inner.backarrow_key_mode,
            ignore_keypad_with_numlock: inner.ignore_keypad_with_numlock,
            alt_esc_prefix: inner.alt_esc_prefix,
            modify_other_keys_state_2: inner.modify_other_keys_state == 2,
            kitty_flags: inner.current_kitty_flags(),
        }
    }

    pub(crate) fn mouse_encoder_state(
        &self,
    ) -> (ffi::GhosttyMouseTrackingMode, ffi::GhosttyMouseFormat) {
        let inner = self.lock_inner();
        (inner.mouse_tracking_mode, inner.mouse_format)
    }

    pub fn active_screen(&self) -> Result<ActiveScreen, Error> {
        Ok(self.lock_inner().active_screen())
    }

    pub fn total_rows(&self) -> Result<usize, Error> {
        let inner = self.lock_inner();
        Ok(inner.term.grid().history_size() + usize::from(inner.logical_rows))
    }

    pub fn scrollback_rows(&self) -> Result<usize, Error> {
        Ok(self.lock_inner().term.grid().history_size())
    }

    pub fn max_scrollback(&self) -> usize {
        self.lock_inner().max_scrollback
    }

    pub fn scrollbar(&self) -> Result<TerminalScrollbar, Error> {
        let inner = self.lock_inner();
        let history = inner.term.grid().history_size();
        let display_offset = inner.term.grid().display_offset().min(history);
        let len = usize::from(inner.logical_rows);
        Ok(TerminalScrollbar {
            total: history + len,
            offset: history - display_offset,
            len,
        })
    }

    #[cfg(windows)]
    pub fn track_row(&mut self, y: u32) -> Option<usize> {
        let inner = self.inner_mut();
        let active = inner.active_screen();
        let history = inner.term.grid().history_size() as i32;
        let rows = i32::from(inner.logical_rows);
        let prev = inner.tracked_grid_line.take().and_then(|(screen, line)| {
            if screen != active {
                return None;
            }
            let screen_y = line + history;
            (screen_y >= 0 && screen_y < history + rows).then_some(screen_y as usize)
        });
        if (y as i32) < rows {
            let display_offset = inner.term.grid().display_offset() as i32;
            inner.tracked_grid_line = Some((active, (y as i32) - display_offset));
        }
        prev
    }

    pub fn scroll_viewport_bottom(&mut self) {
        let inner = self.inner_mut();
        if inner.term.grid().display_offset() != 0 {
            inner.term.scroll_display(Scroll::Bottom);
            inner.mark_full_damage();
        }
    }

    pub fn scroll_viewport_delta(&mut self, delta: isize) {
        if delta == 0 {
            return;
        }
        let inner = self.inner_mut();
        let before = inner.term.grid().display_offset();
        // Negative Ghostty delta scrolls UP toward offset 0 (positive alacritty display_offset delta).
        let alacritty_delta = delta.saturating_neg().clamp(
            isize::try_from(i32::MIN).unwrap_or(-1_000_000),
            isize::try_from(i32::MAX).unwrap_or(1_000_000),
        ) as i32;
        inner.term.scroll_display(Scroll::Delta(alacritty_delta));
        if inner.term.grid().display_offset() != before {
            inner.mark_full_damage();
        }
    }

    pub fn scroll_viewport_row(&mut self, row: usize) {
        let inner = self.inner_mut();
        let history = inner.term.grid().history_size();
        let clamped_row = row.min(history);
        let target_display_offset = history - clamped_row;
        let current = inner.term.grid().display_offset();
        if target_display_offset != current {
            let delta = (target_display_offset as isize) - (current as isize);
            let clamped = delta.clamp(
                isize::try_from(i32::MIN).unwrap_or(-1_000_000),
                isize::try_from(i32::MAX).unwrap_or(1_000_000),
            ) as i32;
            inner.term.scroll_display(Scroll::Delta(clamped));
            inner.mark_full_damage();
        }
    }

    pub fn cols(&self) -> Result<u16, Error> {
        Ok(self.lock_inner().logical_cols)
    }

    pub fn rows(&self) -> Result<u16, Error> {
        Ok(self.lock_inner().logical_rows)
    }

    pub fn cursor_y(&self) -> Result<u16, Error> {
        let inner = self.lock_inner();
        let y = inner.term.grid().cursor.point.line.0.max(0) as u16;
        Ok(y.min(inner.logical_rows.saturating_sub(1)))
    }

    pub fn effective_foreground_color(&self) -> Result<Option<RgbColor>, Error> {
        Ok(self.lock_inner().fg_override)
    }

    pub fn effective_cursor_color(&self) -> Result<Option<RgbColor>, Error> {
        Ok(self.lock_inner().cursor_override)
    }

    pub fn width_px(&self) -> Result<u32, Error> {
        Ok(self.lock_inner().width_px())
    }

    pub fn height_px(&self) -> Result<u32, Error> {
        Ok(self.lock_inner().height_px())
    }

    pub fn kitty_graphics_generation(&self) -> Result<u64, Error> {
        Ok(0)
    }

    pub fn kitty_graphics_may_have_placements(&self) -> Result<bool, Error> {
        Ok(false)
    }

    pub fn kitty_image_fingerprints(&self, image_ids: &[u32]) -> Result<Vec<Option<u64>>, Error> {
        Ok(vec![None; image_ids.len()])
    }

    pub fn kitty_image_placements(&self) -> Result<Vec<KittyImagePlacement>, Error> {
        Ok(Vec::new())
    }

    pub fn kitty_image_placements_with_data_filter<F>(
        &self,
        _needs_data: F,
    ) -> Result<Vec<KittyImagePlacement>, Error>
    where
        F: FnMut(KittyImageDescriptor) -> bool,
    {
        Ok(Vec::new())
    }
}
