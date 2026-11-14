//! Pure-Rust keyboard, mouse, and focus sequence encoders matching Ghostty's VT rules.

use std::fmt::Write as _;

use crate::ffi;
use crate::terminal::Terminal;
use crate::{Error, FocusEvent};

const BINDING_MODS_MASK: u16 = (ffi::GHOSTTY_MODS_SHIFT
    | ffi::GHOSTTY_MODS_CTRL
    | ffi::GHOSTTY_MODS_ALT
    | ffi::GHOSTTY_MODS_SUPER) as u16;

pub fn encode_focus(event: FocusEvent) -> Result<Vec<u8>, Error> {
    Ok(match event {
        FocusEvent::Gained => b"\x1b[I".to_vec(),
        FocusEvent::Lost => b"\x1b[O".to_vec(),
    })
}

#[derive(Debug, Clone)]
pub struct KeyEvent {
    action: ffi::GhosttyKeyAction,
    key: ffi::GhosttyKey,
    mods: u16,
    consumed_mods: u16,
    composing: bool,
    utf8: Box<str>,
    unshifted_codepoint: u32,
}

impl KeyEvent {
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            action: ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_PRESS,
            key: ffi::GhosttyKey_GHOSTTY_KEY_UNIDENTIFIED,
            mods: 0,
            consumed_mods: 0,
            composing: false,
            utf8: Box::from(""),
            unshifted_codepoint: 0,
        })
    }

    pub fn set_action(&mut self, action: ffi::GhosttyKeyAction) {
        self.action = action;
    }

    pub fn set_key(&mut self, key: ffi::GhosttyKey) {
        self.key = key;
    }

    pub fn set_mods(&mut self, mods: u16) {
        self.mods = mods;
    }

    pub fn set_utf8(&mut self, text: &str) {
        self.utf8 = Box::from(text);
    }

    pub fn set_unshifted_codepoint(&mut self, codepoint: u32) {
        self.unshifted_codepoint = codepoint;
    }

    pub fn set_consumed_mods(&mut self, mods: u16) {
        self.consumed_mods = mods;
    }

    pub fn set_composing(&mut self, composing: bool) {
        self.composing = composing;
    }

    fn effective_mods(&self) -> u16 {
        if self.utf8.is_empty() {
            self.mods
        } else {
            self.mods & !self.consumed_mods
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct KeyEncoder {
    cursor_key_application: bool,
    keypad_key_application: bool,
    backarrow_key_mode: bool,
    ignore_keypad_with_numlock: bool,
    alt_esc_prefix: bool,
    modify_other_keys_state_2: bool,
    kitty_flags: u8,
    macos_option_as_alt: bool,
}

impl KeyEncoder {
    pub fn new() -> Result<Self, Error> {
        Ok(Self::default())
    }

    pub fn set_from_terminal(&mut self, terminal: &Terminal) {
        let state = terminal.key_encoder_state();
        self.cursor_key_application = state.cursor_key_application;
        self.keypad_key_application = state.keypad_key_application;
        self.backarrow_key_mode = state.backarrow_key_mode;
        self.ignore_keypad_with_numlock = state.ignore_keypad_with_numlock;
        self.alt_esc_prefix = state.alt_esc_prefix;
        self.modify_other_keys_state_2 = state.modify_other_keys_state_2;
        self.kitty_flags = state.kitty_flags;
        // Matches Ghostty's `Options.fromTerminal`: host Option-as-Alt is not
        // stored in terminal state and resets to false until re-applied.
        self.macos_option_as_alt = false;
    }

    /// Whether macOS Option acts as Alt (ESC prefix) rather than a text
    /// modifier. `set_from_terminal` resets this to false.
    pub fn set_macos_option_as_alt(&mut self, enabled: bool) {
        self.macos_option_as_alt = enabled;
    }

    pub fn set_cursor_key_application(&mut self, enabled: bool) {
        self.cursor_key_application = enabled;
    }

    pub fn set_keypad_key_application(&mut self, enabled: bool) {
        self.keypad_key_application = enabled;
    }

    pub fn set_backarrow_key_mode(&mut self, enabled: bool) {
        self.backarrow_key_mode = enabled;
    }

    pub fn set_ignore_keypad_with_numlock(&mut self, enabled: bool) {
        self.ignore_keypad_with_numlock = enabled;
    }

    pub fn set_alt_esc_prefix(&mut self, enabled: bool) {
        self.alt_esc_prefix = enabled;
    }

    pub fn set_modify_other_keys_state_2(&mut self, enabled: bool) {
        self.modify_other_keys_state_2 = enabled;
    }

    pub fn set_kitty_flags(&mut self, flags: u8) {
        self.kitty_flags = flags & ffi::GHOSTTY_KITTY_KEY_ALL;
    }

    pub fn encode(&mut self, event: &KeyEvent) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        if self.kitty_flags != 0 {
            self.encode_kitty(event, &mut out);
        } else {
            self.encode_legacy(event, &mut out);
        }
        Ok(out)
    }

    fn encode_kitty(&self, event: &KeyEvent, out: &mut Vec<u8>) {
        let report_events = self.kitty_flags & ffi::GHOSTTY_KITTY_KEY_REPORT_EVENTS != 0;
        let report_alternates = self.kitty_flags & ffi::GHOSTTY_KITTY_KEY_REPORT_ALTERNATES != 0;
        let report_all = self.kitty_flags & ffi::GHOSTTY_KITTY_KEY_REPORT_ALL != 0;
        let report_associated = self.kitty_flags & ffi::GHOSTTY_KITTY_KEY_REPORT_ASSOCIATED != 0;

        let is_release = event.action == ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_RELEASE;
        if is_release {
            if !report_events {
                return;
            }
            if !report_all
                && matches!(
                    event.key,
                    ffi::GhosttyKey_GHOSTTY_KEY_ENTER
                        | ffi::GhosttyKey_GHOSTTY_KEY_BACKSPACE
                        | ffi::GhosttyKey_GHOSTTY_KEY_TAB
                )
            {
                return;
            }
        }

        let all_mods = event.mods;
        let binding_mods = event.effective_mods() & BINDING_MODS_MASK;
        let entry = lookup_kitty_entry(event.key).or_else(|| {
            (event.unshifted_codepoint > 0).then_some(KittyEntry {
                code: event.unshifted_codepoint,
                final_byte: b'u',
                modifier: false,
            })
        });

        'preprocessing: {
            if event.composing {
                if entry.is_some_and(|e| e.modifier) {
                    break 'preprocessing;
                }
                return;
            }

            if !event.utf8.is_empty()
                && matches!(
                    event.key,
                    ffi::GhosttyKey_GHOSTTY_KEY_ENTER | ffi::GhosttyKey_GHOSTTY_KEY_BACKSPACE
                )
                && !is_control_utf8(&event.utf8)
            {
                if event.key == ffi::GhosttyKey_GHOSTTY_KEY_BACKSPACE {
                    return;
                }
                out.extend_from_slice(event.utf8.as_bytes());
                return;
            }

            if !report_all {
                if binding_mods == 0 {
                    match event.key {
                        ffi::GhosttyKey_GHOSTTY_KEY_ENTER => {
                            out.push(b'\r');
                            return;
                        }
                        ffi::GhosttyKey_GHOSTTY_KEY_TAB => {
                            out.push(b'\t');
                            return;
                        }
                        ffi::GhosttyKey_GHOSTTY_KEY_BACKSPACE => {
                            out.push(0x7f);
                            return;
                        }
                        _ => {}
                    }
                }

                if !event.utf8.is_empty()
                    && binding_mods == 0
                    && !is_release
                    && !event.utf8.chars().any(|c| is_control_codepoint(c as u32))
                {
                    out.extend_from_slice(event.utf8.as_bytes());
                    return;
                }
            }
        }

        let Some(entry) = entry else {
            if !is_release && !event.utf8.is_empty() {
                out.extend_from_slice(event.utf8.as_bytes());
            }
            return;
        };

        if entry.modifier && !report_all {
            return;
        }

        let kitty_mods = kitty_mods_bits(all_mods);
        let mods_seq = u16::from(kitty_mods) + 1;
        let event_code: u8 = if report_events {
            match event.action {
                ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_PRESS => 1,
                ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_REPEAT => 2,
                ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_RELEASE => 3,
                _ => 0,
            }
        } else {
            0
        };

        let mut alternates: [Option<u32>; 2] = [None, None];
        if report_alternates && !is_control_codepoint(entry.code) {
            let mut chars = event.utf8.chars();
            if let Some(cp1_char) = chars.next() {
                let cp1 = cp1_char as u32;
                let has_shift = all_mods & (ffi::GHOSTTY_MODS_SHIFT as u16) != 0;
                if cp1 != entry.code && has_shift {
                    alternates[0] = Some(cp1);
                }
                let has_cp2 = chars.next().is_some();
                if let Some(base) = key_codepoint(event.key) {
                    if base != entry.code && cp1 != base && !has_cp2 {
                        alternates[1] = Some(base);
                    }
                }
            } else if let Some(base) = key_codepoint(event.key) {
                if base != entry.code {
                    alternates[1] = Some(base);
                }
            }
        }

        let associated_text = if report_associated && event_code != 3 {
            let alt_prevents_text = if cfg!(target_os = "macos") {
                self.macos_option_as_alt
            } else {
                true
            };
            let has_alt = all_mods & (ffi::GHOSTTY_MODS_ALT as u16) != 0;
            let has_ctrl = all_mods & (ffi::GHOSTTY_MODS_CTRL as u16) != 0;
            let has_super = all_mods & (ffi::GHOSTTY_MODS_SUPER as u16) != 0;
            if (has_alt && alt_prevents_text) || has_ctrl || has_super {
                ""
            } else {
                &event.utf8
            }
        } else {
            ""
        };

        if entry.final_byte == b'u' || entry.final_byte == b'~' {
            let mut s = String::with_capacity(24);
            let _ = write!(s, "\x1b[{}", entry.code);
            if let Some(shifted) = alternates[0] {
                let _ = write!(s, ":{shifted}");
            }
            if let Some(base) = alternates[1] {
                if alternates[0].is_none() {
                    let _ = write!(s, "::{base}");
                } else {
                    let _ = write!(s, ":{base}");
                }
            }

            let mut emit_prior = false;
            if event_code != 0 && event_code != 1 {
                let _ = write!(s, ";{mods_seq}:{event_code}");
                emit_prior = true;
            } else if mods_seq > 1 {
                let _ = write!(s, ";{mods_seq}");
                emit_prior = true;
            }

            if !associated_text.is_empty() {
                let mut count = 0usize;
                for ch in associated_text.chars() {
                    let cp = ch as u32;
                    if is_control_codepoint(cp) {
                        continue;
                    }
                    if count == 0 {
                        if !emit_prior {
                            s.push(';');
                        }
                        s.push(';');
                    } else {
                        s.push(':');
                    }
                    let _ = write!(s, "{cp}");
                    count += 1;
                }
            }

            s.push(char::from(entry.final_byte));
            out.extend_from_slice(s.as_bytes());
        } else {
            let mut s = String::with_capacity(16);
            let final_ch = char::from(entry.final_byte);
            if event_code != 0 {
                let _ = write!(s, "\x1b[1;{mods_seq}:{event_code}{final_ch}");
            } else if mods_seq > 1 {
                let _ = write!(s, "\x1b[1;{mods_seq}{final_ch}");
            } else {
                let _ = write!(s, "\x1b[{final_ch}");
            }
            out.extend_from_slice(s.as_bytes());
        }
    }

    fn encode_legacy(&self, event: &KeyEvent, out: &mut Vec<u8>) {
        if event.action != ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_PRESS
            && event.action != ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_REPEAT
        {
            return;
        }
        if event.composing {
            return;
        }

        let all_mods = event.mods;
        let binding_mods = event.effective_mods() & BINDING_MODS_MASK;

        if let Some(seq) = pc_style_function_key(
            event.key,
            all_mods,
            self.cursor_key_application,
            self.keypad_key_application,
            self.ignore_keypad_with_numlock,
            self.modify_other_keys_state_2,
            self.backarrow_key_mode,
        ) {
            let mut skip_pc_style = false;
            if !event.utf8.is_empty()
                && matches!(
                    event.key,
                    ffi::GhosttyKey_GHOSTTY_KEY_BACKSPACE
                        | ffi::GhosttyKey_GHOSTTY_KEY_ENTER
                        | ffi::GhosttyKey_GHOSTTY_KEY_ESCAPE
                )
                && !is_control_utf8(&event.utf8)
            {
                if event.key == ffi::GhosttyKey_GHOSTTY_KEY_BACKSPACE {
                    return;
                }
                skip_pc_style = true;
            }
            if !skip_pc_style {
                out.extend_from_slice(&seq);
                return;
            }
        }

        let utf8 = &*event.utf8;
        if self.modify_other_keys_state_2 {
            let mut chars = utf8.chars();
            if let (Some(ch), None) = (chars.next(), chars.next()) {
                let codepoint = ch as u32;
                let mut mods = all_mods & BINDING_MODS_MASK;
                if cfg!(target_os = "macos") && !self.macos_option_as_alt {
                    mods &= !(ffi::GHOSTTY_MODS_ALT as u16);
                }
                let mods_no_shift = mods & !(ffi::GHOSTTY_MODS_SHIFT as u16);
                let should_modify = (0x40..=0x7f).contains(&codepoint)
                    || mods_no_shift != 0
                    || codepoint == b' ' as u32;
                if should_modify {
                    if let Some(code) = xterm_modifier_code(mods) {
                        let mut s = String::with_capacity(16);
                        let _ = write!(s, "\x1b[27;{code};{codepoint}~");
                        out.extend_from_slice(s.as_bytes());
                        return;
                    }
                }
            }
        }

        if let Some(ctrl_byte) = ctrl_seq(event.key, utf8, event.unshifted_codepoint, all_mods) {
            if binding_mods & (ffi::GHOSTTY_MODS_ALT as u16) != 0 {
                out.push(0x1b);
            }
            out.push(ctrl_byte);
            return;
        }

        if utf8.is_empty() {
            let _ = self.legacy_alt_prefix(event, binding_mods, out);
            return;
        }

        if all_mods & (ffi::GHOSTTY_MODS_CTRL as u16) != 0 {
            let mut chars = utf8.chars();
            if let (Some(ch), None) = (chars.next(), chars.next()) {
                let mut cp = ch as u32;
                let mut shift = all_mods & (ffi::GHOSTTY_MODS_SHIFT as u16) != 0;
                let alt = all_mods & (ffi::GHOSTTY_MODS_ALT as u16) != 0;
                let ctrl = all_mods & (ffi::GHOSTTY_MODS_CTRL as u16) != 0;
                if (b'A' as u32..=b'Z' as u32).contains(&cp) && shift {
                    cp = cp - b'A' as u32 + b'a' as u32;
                }
                if event.unshifted_codepoint != cp {
                    shift = false;
                }
                let mut csi_u_bits = 0u8;
                if shift {
                    csi_u_bits |= 1;
                }
                if alt {
                    csi_u_bits |= 2;
                }
                if ctrl {
                    csi_u_bits |= 4;
                }
                let seq_mods = csi_u_bits + 1;
                let mut s = String::with_capacity(16);
                let _ = write!(s, "\x1b[{cp};{seq_mods}u");
                out.extend_from_slice(s.as_bytes());
                return;
            }
        }

        if self.legacy_alt_prefix(event, binding_mods, out) {
            return;
        }

        #[cfg(target_os = "macos")]
        if all_mods & (ffi::GHOSTTY_MODS_SUPER as u16) != 0 {
            return;
        }

        out.extend_from_slice(utf8.as_bytes());
    }

    fn legacy_alt_prefix(&self, event: &KeyEvent, binding_mods: u16, out: &mut Vec<u8>) -> bool {
        if binding_mods & (ffi::GHOSTTY_MODS_ALT as u16) == 0 || !self.alt_esc_prefix {
            return false;
        }
        if cfg!(target_os = "macos") && !self.macos_option_as_alt {
            return false;
        }

        let utf8 = event.utf8.as_bytes();
        if utf8.len() == 1 {
            out.push(0x1b);
            out.extend_from_slice(utf8);
            return true;
        }

        if cfg!(target_os = "macos") && event.unshifted_codepoint > 0 {
            let Some(ch) = char::from_u32(event.unshifted_codepoint) else {
                return false;
            };
            let mut buf = [0u8; 4];
            let encoded = ch.encode_utf8(&mut buf);
            out.push(0x1b);
            out.extend_from_slice(encoded.as_bytes());
            return true;
        }

        if !utf8.is_empty() {
            out.push(0x1b);
            out.extend_from_slice(utf8);
            return true;
        }

        if event.unshifted_codepoint > 0 {
            let Some(ch) = char::from_u32(event.unshifted_codepoint) else {
                return false;
            };
            let mut buf = [0u8; 4];
            let encoded = ch.encode_utf8(&mut buf);
            out.push(0x1b);
            out.extend_from_slice(encoded.as_bytes());
            return true;
        }

        false
    }
}

#[derive(Clone, Copy)]
struct KittyEntry {
    code: u32,
    final_byte: u8,
    modifier: bool,
}

fn lookup_kitty_entry(key: ffi::GhosttyKey) -> Option<KittyEntry> {
    let (code, final_byte, modifier) = match key {
        ffi::GhosttyKey_GHOSTTY_KEY_ESCAPE => (27, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_ENTER => (13, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_TAB => (9, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_BACKSPACE => (127, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_INSERT => (2, b'~', false),
        ffi::GhosttyKey_GHOSTTY_KEY_DELETE => (3, b'~', false),
        ffi::GhosttyKey_GHOSTTY_KEY_ARROW_LEFT => (1, b'D', false),
        ffi::GhosttyKey_GHOSTTY_KEY_ARROW_RIGHT => (1, b'C', false),
        ffi::GhosttyKey_GHOSTTY_KEY_ARROW_UP => (1, b'A', false),
        ffi::GhosttyKey_GHOSTTY_KEY_ARROW_DOWN => (1, b'B', false),
        ffi::GhosttyKey_GHOSTTY_KEY_PAGE_UP => (5, b'~', false),
        ffi::GhosttyKey_GHOSTTY_KEY_PAGE_DOWN => (6, b'~', false),
        ffi::GhosttyKey_GHOSTTY_KEY_HOME => (1, b'H', false),
        ffi::GhosttyKey_GHOSTTY_KEY_END => (1, b'F', false),
        ffi::GhosttyKey_GHOSTTY_KEY_CAPS_LOCK => (57358, b'u', true),
        ffi::GhosttyKey_GHOSTTY_KEY_SCROLL_LOCK => (57359, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUM_LOCK => (57360, b'u', true),
        ffi::GhosttyKey_GHOSTTY_KEY_PRINT_SCREEN => (57361, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_PAUSE => (57362, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_F1 => (1, b'P', false),
        ffi::GhosttyKey_GHOSTTY_KEY_F2 => (1, b'Q', false),
        ffi::GhosttyKey_GHOSTTY_KEY_F3 => (13, b'~', false),
        ffi::GhosttyKey_GHOSTTY_KEY_F4 => (1, b'S', false),
        ffi::GhosttyKey_GHOSTTY_KEY_F5 => (15, b'~', false),
        ffi::GhosttyKey_GHOSTTY_KEY_F6 => (17, b'~', false),
        ffi::GhosttyKey_GHOSTTY_KEY_F7 => (18, b'~', false),
        ffi::GhosttyKey_GHOSTTY_KEY_F8 => (19, b'~', false),
        ffi::GhosttyKey_GHOSTTY_KEY_F9 => (20, b'~', false),
        ffi::GhosttyKey_GHOSTTY_KEY_F10 => (21, b'~', false),
        ffi::GhosttyKey_GHOSTTY_KEY_F11 => (23, b'~', false),
        ffi::GhosttyKey_GHOSTTY_KEY_F12 => (24, b'~', false),
        ffi::GhosttyKey_GHOSTTY_KEY_F13 => (57376, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_F14 => (57377, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_F15 => (57378, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_F16 => (57379, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_F17 => (57380, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_F18 => (57381, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_F19 => (57382, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_F20 => (57383, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_F21 => (57384, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_F22 => (57385, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_F23 => (57386, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_F24 => (57387, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_F25 => (57388, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_0 => (57399, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_1 => (57400, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_2 => (57401, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_3 => (57402, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_4 => (57403, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_5 => (57404, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_6 => (57405, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_7 => (57406, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_8 => (57407, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_9 => (57408, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_DECIMAL => (57409, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_DIVIDE => (57410, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_MULTIPLY => (57411, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_SUBTRACT => (57412, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_ADD => (57413, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_ENTER => (57414, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_EQUAL => (57415, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_SEPARATOR => (57416, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_LEFT => (57417, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_RIGHT => (57418, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_UP => (57419, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_DOWN => (57420, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_PAGE_UP => (57421, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_PAGE_DOWN => (57422, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_HOME => (57423, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_END => (57424, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_INSERT => (57425, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_DELETE => (57426, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_BEGIN => (57427, b'u', false),
        ffi::GhosttyKey_GHOSTTY_KEY_SHIFT_LEFT => (57441, b'u', true),
        ffi::GhosttyKey_GHOSTTY_KEY_SHIFT_RIGHT => (57447, b'u', true),
        ffi::GhosttyKey_GHOSTTY_KEY_CONTROL_LEFT => (57442, b'u', true),
        ffi::GhosttyKey_GHOSTTY_KEY_CONTROL_RIGHT => (57448, b'u', true),
        ffi::GhosttyKey_GHOSTTY_KEY_META_LEFT => (57444, b'u', true),
        ffi::GhosttyKey_GHOSTTY_KEY_META_RIGHT => (57450, b'u', true),
        ffi::GhosttyKey_GHOSTTY_KEY_ALT_LEFT => (57443, b'u', true),
        ffi::GhosttyKey_GHOSTTY_KEY_ALT_RIGHT => (57449, b'u', true),
        _ => return None,
    };
    Some(KittyEntry {
        code,
        final_byte,
        modifier,
    })
}

fn key_codepoint(key: ffi::GhosttyKey) -> Option<u32> {
    let ch = match key {
        ffi::GhosttyKey_GHOSTTY_KEY_A..=ffi::GhosttyKey_GHOSTTY_KEY_Z => {
            char::from(b'a' + (key - ffi::GhosttyKey_GHOSTTY_KEY_A) as u8)
        }
        ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_0..=ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_9 => {
            char::from(b'0' + (key - ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_0) as u8)
        }
        ffi::GhosttyKey_GHOSTTY_KEY_SEMICOLON => ';',
        ffi::GhosttyKey_GHOSTTY_KEY_SPACE => ' ',
        ffi::GhosttyKey_GHOSTTY_KEY_QUOTE => '\'',
        ffi::GhosttyKey_GHOSTTY_KEY_COMMA => ',',
        ffi::GhosttyKey_GHOSTTY_KEY_BACKQUOTE => '`',
        ffi::GhosttyKey_GHOSTTY_KEY_PERIOD => '.',
        ffi::GhosttyKey_GHOSTTY_KEY_SLASH => '/',
        ffi::GhosttyKey_GHOSTTY_KEY_MINUS => '-',
        ffi::GhosttyKey_GHOSTTY_KEY_EQUAL => '=',
        ffi::GhosttyKey_GHOSTTY_KEY_BRACKET_LEFT => '[',
        ffi::GhosttyKey_GHOSTTY_KEY_BRACKET_RIGHT => ']',
        ffi::GhosttyKey_GHOSTTY_KEY_BACKSLASH => '\\',
        ffi::GhosttyKey_GHOSTTY_KEY_TAB => '\t',
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_0..=ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_9 => {
            char::from(b'0' + (key - ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_0) as u8)
        }
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_DECIMAL => '.',
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_DIVIDE => '/',
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_MULTIPLY => '*',
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_SUBTRACT => '-',
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_ADD => '+',
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_EQUAL => '=',
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_COMMA => ',',
        _ => return None,
    };
    Some(ch as u32)
}

fn kitty_mods_bits(mods: u16) -> u8 {
    let mut bits = 0u8;
    if mods & (ffi::GHOSTTY_MODS_SHIFT as u16) != 0 {
        bits |= 1;
    }
    if mods & (ffi::GHOSTTY_MODS_ALT as u16) != 0 {
        bits |= 2;
    }
    if mods & (ffi::GHOSTTY_MODS_CTRL as u16) != 0 {
        bits |= 4;
    }
    if mods & (ffi::GHOSTTY_MODS_SUPER as u16) != 0 {
        bits |= 8;
    }
    if mods & (ffi::GHOSTTY_MODS_CAPS_LOCK as u16) != 0 {
        bits |= 64;
    }
    if mods & (ffi::GHOSTTY_MODS_NUM_LOCK as u16) != 0 {
        bits |= 128;
    }
    bits
}

fn xterm_modifier_code(binding_mods: u16) -> Option<u8> {
    let shift = binding_mods & (ffi::GHOSTTY_MODS_SHIFT as u16) != 0;
    let alt = binding_mods & (ffi::GHOSTTY_MODS_ALT as u16) != 0;
    let ctrl = binding_mods & (ffi::GHOSTTY_MODS_CTRL as u16) != 0;
    let sup = binding_mods & (ffi::GHOSTTY_MODS_SUPER as u16) != 0;
    let bits =
        u8::from(shift) | (u8::from(alt) << 1) | (u8::from(ctrl) << 2) | (u8::from(sup) << 3);
    (bits != 0).then_some(bits + 1)
}

#[allow(clippy::too_many_arguments)] // Mirrors Ghostty's `pcStyleFunctionKey` parameter set directly.
fn pc_style_function_key(
    key: ffi::GhosttyKey,
    mods: u16,
    cursor_key_application: bool,
    keypad_key_application_req: bool,
    ignore_keypad_with_numlock: bool,
    modify_other_keys_2: bool,
    backarrow_key_mode: bool,
) -> Option<Vec<u8>> {
    let binding_mods = mods & BINDING_MODS_MASK;
    let mod_code = xterm_modifier_code(binding_mods);
    let keypad_app = if ignore_keypad_with_numlock {
        false
    } else {
        keypad_key_application_req
    };

    let shift = binding_mods == (ffi::GHOSTTY_MODS_SHIFT as u16);
    let alt = binding_mods == (ffi::GHOSTTY_MODS_ALT as u16);
    let ctrl = binding_mods == (ffi::GHOSTTY_MODS_CTRL as u16);

    match key {
        ffi::GhosttyKey_GHOSTTY_KEY_ARROW_UP | ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_UP => {
            Some(cursor_or_pc_seq(b'A', mod_code, cursor_key_application))
        }
        ffi::GhosttyKey_GHOSTTY_KEY_ARROW_DOWN | ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_DOWN => {
            Some(cursor_or_pc_seq(b'B', mod_code, cursor_key_application))
        }
        ffi::GhosttyKey_GHOSTTY_KEY_ARROW_RIGHT | ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_RIGHT => {
            Some(cursor_or_pc_seq(b'C', mod_code, cursor_key_application))
        }
        ffi::GhosttyKey_GHOSTTY_KEY_ARROW_LEFT | ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_LEFT => {
            Some(cursor_or_pc_seq(b'D', mod_code, cursor_key_application))
        }
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_BEGIN => {
            Some(cursor_or_pc_seq(b'E', mod_code, cursor_key_application))
        }
        ffi::GhosttyKey_GHOSTTY_KEY_HOME | ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_HOME => {
            Some(cursor_or_pc_seq(b'H', mod_code, cursor_key_application))
        }
        ffi::GhosttyKey_GHOSTTY_KEY_END | ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_END => {
            Some(cursor_or_pc_seq(b'F', mod_code, cursor_key_application))
        }
        ffi::GhosttyKey_GHOSTTY_KEY_INSERT | ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_INSERT => {
            Some(tilde_pc_seq(2, mod_code))
        }
        ffi::GhosttyKey_GHOSTTY_KEY_DELETE | ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_DELETE => {
            Some(tilde_pc_seq(3, mod_code))
        }
        ffi::GhosttyKey_GHOSTTY_KEY_PAGE_UP | ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_PAGE_UP => {
            Some(tilde_pc_seq(5, mod_code))
        }
        ffi::GhosttyKey_GHOSTTY_KEY_PAGE_DOWN | ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_PAGE_DOWN => {
            Some(tilde_pc_seq(6, mod_code))
        }
        ffi::GhosttyKey_GHOSTTY_KEY_HELP => Some(tilde_pc_seq(28, mod_code)),
        ffi::GhosttyKey_GHOSTTY_KEY_CONTEXT_MENU => Some(tilde_pc_seq(29, mod_code)),
        ffi::GhosttyKey_GHOSTTY_KEY_F1 => Some(ss3_or_pc_seq(b'P', mod_code)),
        ffi::GhosttyKey_GHOSTTY_KEY_F2 => Some(ss3_or_pc_seq(b'Q', mod_code)),
        ffi::GhosttyKey_GHOSTTY_KEY_F3 => Some(if let Some(code) = mod_code {
            format!("\x1b[13;{code}~").into_bytes()
        } else {
            b"\x1bOR".to_vec()
        }),
        ffi::GhosttyKey_GHOSTTY_KEY_F4 => Some(ss3_or_pc_seq(b'S', mod_code)),
        ffi::GhosttyKey_GHOSTTY_KEY_F5 => Some(tilde_pc_seq(15, mod_code)),
        ffi::GhosttyKey_GHOSTTY_KEY_F6 => Some(tilde_pc_seq(17, mod_code)),
        ffi::GhosttyKey_GHOSTTY_KEY_F7 => Some(tilde_pc_seq(18, mod_code)),
        ffi::GhosttyKey_GHOSTTY_KEY_F8 => Some(tilde_pc_seq(19, mod_code)),
        ffi::GhosttyKey_GHOSTTY_KEY_F9 => Some(tilde_pc_seq(20, mod_code)),
        ffi::GhosttyKey_GHOSTTY_KEY_F10 => Some(tilde_pc_seq(21, mod_code)),
        ffi::GhosttyKey_GHOSTTY_KEY_F11 => Some(tilde_pc_seq(23, mod_code)),
        ffi::GhosttyKey_GHOSTTY_KEY_F12 => Some(tilde_pc_seq(24, mod_code)),
        ffi::GhosttyKey_GHOSTTY_KEY_F13 => Some(tilde_pc_seq(25, mod_code)),
        ffi::GhosttyKey_GHOSTTY_KEY_F14 => Some(tilde_pc_seq(26, mod_code)),
        ffi::GhosttyKey_GHOSTTY_KEY_F15 => Some(tilde_pc_seq(28, mod_code)),
        ffi::GhosttyKey_GHOSTTY_KEY_F16 => Some(tilde_pc_seq(29, mod_code)),
        ffi::GhosttyKey_GHOSTTY_KEY_F17 => Some(tilde_pc_seq(31, mod_code)),
        ffi::GhosttyKey_GHOSTTY_KEY_F18 => Some(tilde_pc_seq(32, mod_code)),
        ffi::GhosttyKey_GHOSTTY_KEY_F19 => Some(tilde_pc_seq(33, mod_code)),
        ffi::GhosttyKey_GHOSTTY_KEY_F20 => Some(tilde_pc_seq(34, mod_code)),
        ffi::GhosttyKey_GHOSTTY_KEY_F21 => Some(tilde_pc_seq(42, mod_code)),
        ffi::GhosttyKey_GHOSTTY_KEY_F22 => Some(tilde_pc_seq(43, mod_code)),
        ffi::GhosttyKey_GHOSTTY_KEY_F23 => Some(tilde_pc_seq(44, mod_code)),
        ffi::GhosttyKey_GHOSTTY_KEY_F24 => Some(tilde_pc_seq(45, mod_code)),
        ffi::GhosttyKey_GHOSTTY_KEY_F25 => Some(tilde_pc_seq(46, mod_code)),

        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_0 => Some(kp_seq(b'p', b"0", mod_code, keypad_app)),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_1 => Some(kp_seq(b'q', b"1", mod_code, keypad_app)),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_2 => Some(kp_seq(b'r', b"2", mod_code, keypad_app)),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_3 => Some(kp_seq(b's', b"3", mod_code, keypad_app)),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_4 => Some(kp_seq(b't', b"4", mod_code, keypad_app)),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_5 => Some(kp_seq(b'u', b"5", mod_code, keypad_app)),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_6 => Some(kp_seq(b'v', b"6", mod_code, keypad_app)),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_7 => Some(kp_seq(b'w', b"7", mod_code, keypad_app)),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_8 => Some(kp_seq(b'x', b"8", mod_code, keypad_app)),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_9 => Some(kp_seq(b'y', b"9", mod_code, keypad_app)),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_DECIMAL => {
            Some(kp_seq(b'n', b".", mod_code, keypad_app))
        }
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_DIVIDE => Some(kp_seq(b'o', b"/", mod_code, keypad_app)),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_MULTIPLY => {
            Some(kp_seq(b'j', b"*", mod_code, keypad_app))
        }
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_SUBTRACT => {
            Some(kp_seq(b'm', b"-", mod_code, keypad_app))
        }
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_ADD => Some(kp_seq(b'k', b"+", mod_code, keypad_app)),
        ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_ENTER => Some(kp_seq(b'M', b"\r", mod_code, keypad_app)),

        ffi::GhosttyKey_GHOSTTY_KEY_BACKSPACE => {
            if ctrl {
                return Some(if backarrow_key_mode {
                    b"\x7f".to_vec()
                } else {
                    b"\x08".to_vec()
                });
            }
            if binding_mods == 0 {
                return Some(if backarrow_key_mode {
                    b"\x08".to_vec()
                } else {
                    b"\x7f".to_vec()
                });
            }
            if modify_other_keys_2 {
                if let Some(code) = mod_code {
                    return Some(format!("\x1b[27;{code};127~").into_bytes());
                }
            }
            // In non-mode-2, Alt+Ctrl+Shift ( without Super ) falls through to default
            // backspace in `function_keys.zig`! Check exact entries in `function_keys.zig`:
            let has_alt = binding_mods & (ffi::GHOSTTY_MODS_ALT as u16) != 0;
            let has_ctrl = binding_mods & (ffi::GHOSTTY_MODS_CTRL as u16) != 0;
            let has_shift = binding_mods & (ffi::GHOSTTY_MODS_SHIFT as u16) != 0;
            let has_super = binding_mods & (ffi::GHOSTTY_MODS_SUPER as u16) != 0;
            if has_alt && has_ctrl && has_shift && !has_super {
                return Some(if backarrow_key_mode {
                    b"\x08".to_vec()
                } else {
                    b"\x7f".to_vec()
                });
            }
            let base = if has_ctrl { 0x08u8 } else { 0x7fu8 };
            Some(if has_alt {
                vec![0x1b, base]
            } else {
                vec![base]
            })
        }

        ffi::GhosttyKey_GHOSTTY_KEY_TAB => {
            if binding_mods == 0 {
                return Some(b"\t".to_vec());
            }
            if !modify_other_keys_2 {
                if shift {
                    return Some(b"\x1b[Z".to_vec());
                }
                if alt {
                    return Some(b"\x1b\t".to_vec());
                }
            }
            mod_code.map(|code| format!("\x1b[27;{code};9~").into_bytes())
        }

        ffi::GhosttyKey_GHOSTTY_KEY_ENTER => {
            if binding_mods == 0 {
                return Some(b"\r".to_vec());
            }
            if alt && !modify_other_keys_2 {
                return Some(b"\x1b\r".to_vec());
            }
            mod_code.map(|code| format!("\x1b[27;{code};13~").into_bytes())
        }

        ffi::GhosttyKey_GHOSTTY_KEY_ESCAPE => {
            if binding_mods == 0 {
                return Some(b"\x1b".to_vec());
            }
            if alt && !modify_other_keys_2 {
                return Some(b"\x1b\x1b".to_vec());
            }
            mod_code.map(|code| format!("\x1b[27;{code};27~").into_bytes())
        }

        _ => None,
    }
}

fn cursor_or_pc_seq(final_byte: u8, mod_code: Option<u8>, application: bool) -> Vec<u8> {
    let ch = char::from(final_byte);
    if let Some(code) = mod_code {
        format!("\x1b[1;{code}{ch}").into_bytes()
    } else if application {
        vec![0x1b, b'O', final_byte]
    } else {
        vec![0x1b, b'[', final_byte]
    }
}

fn ss3_or_pc_seq(final_byte: u8, mod_code: Option<u8>) -> Vec<u8> {
    let ch = char::from(final_byte);
    if let Some(code) = mod_code {
        format!("\x1b[1;{code}{ch}").into_bytes()
    } else {
        vec![0x1b, b'O', final_byte]
    }
}

fn tilde_pc_seq(num: u8, mod_code: Option<u8>) -> Vec<u8> {
    if let Some(code) = mod_code {
        format!("\x1b[{num};{code}~").into_bytes()
    } else {
        format!("\x1b[{num}~").into_bytes()
    }
}

fn kp_seq(suffix: u8, normal: &[u8], mod_code: Option<u8>, application: bool) -> Vec<u8> {
    if application {
        let ch = char::from(suffix);
        if let Some(code) = mod_code {
            format!("\x1bO{code}{ch}").into_bytes()
        } else {
            vec![0x1b, b'O', suffix]
        }
    } else {
        normal.to_vec()
    }
}

fn ctrl_seq(
    logical_key: ffi::GhosttyKey,
    utf8: &str,
    unshifted_codepoint: u32,
    mods: u16,
) -> Option<u8> {
    let ctrl_bit = ffi::GHOSTTY_MODS_CTRL as u16;
    if mods & ctrl_bit == 0 {
        return None;
    }

    let mut unset_mods = (mods & BINDING_MODS_MASK) & !(ffi::GHOSTTY_MODS_ALT as u16);
    let mut ch: u8 = if utf8.len() == 1 {
        utf8.as_bytes()[0]
    } else if let Some(cp) = key_codepoint(logical_key).and_then(|cp| u8::try_from(cp).ok()) {
        if unset_mods != ctrl_bit {
            return None;
        }
        cp
    } else {
        return None;
    };

    if unset_mods & (ffi::GHOSTTY_MODS_SHIFT as u16) != 0 && !ch.is_ascii_uppercase() && ch != b'@'
    {
        unset_mods &= !(ffi::GHOSTTY_MODS_SHIFT as u16);
    }

    if ch.is_ascii_uppercase() && unshifted_codepoint > 0 {
        if let Ok(byte) = u8::try_from(unshifted_codepoint) {
            ch = byte;
        }
    }

    if unset_mods != ctrl_bit {
        return None;
    }

    match ch {
        b' ' | b'2' | b'@' => Some(0),
        b'/' | b'7' | b'_' => Some(31),
        b'0' => Some(48),
        b'1' => Some(49),
        b'3' => Some(27),
        b'4' | b'\\' => Some(28),
        b'5' | b']' => Some(29),
        b'6' | b'^' | b'~' => Some(30),
        b'8' | b'?' => Some(127),
        b'9' => Some(57),
        b'a'..=b'h' | b'j'..=b'l' | b'n'..=b'z' => Some(ch - b'a' + 1),
        // 'i', 'm', '[' intentionally fall through to fixterms CSI u.
        _ => None,
    }
}

fn is_control_codepoint(cp: u32) -> bool {
    cp < 0x20 || cp == 0x7f
}

fn is_control_utf8(s: &str) -> bool {
    s.len() == 1 && is_control_codepoint(u32::from(s.as_bytes()[0]))
}

#[derive(Debug, Clone, Copy)]
pub struct MouseEvent {
    action: ffi::GhosttyMouseAction,
    button: Option<ffi::GhosttyMouseButton>,
    mods: u16,
    x: f32,
    y: f32,
}

impl MouseEvent {
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            action: ffi::GhosttyMouseAction_GHOSTTY_MOUSE_ACTION_PRESS,
            button: None,
            mods: 0,
            x: 0.0,
            y: 0.0,
        })
    }

    pub fn set_action(&mut self, action: ffi::GhosttyMouseAction) {
        self.action = action;
    }

    pub fn set_button(&mut self, button: ffi::GhosttyMouseButton) {
        self.button =
            (button != ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_UNKNOWN).then_some(button);
    }

    pub fn clear_button(&mut self) {
        self.button = None;
    }

    pub fn set_mods(&mut self, mods: u16) {
        self.mods = mods;
    }

    pub fn set_position(&mut self, x: f32, y: f32) {
        self.x = x;
        self.y = y;
    }
}

#[derive(Debug, Clone, Copy)]
struct MouseSize {
    screen_width: u32,
    screen_height: u32,
    cell_width: u32,
    cell_height: u32,
}

impl Default for MouseSize {
    fn default() -> Self {
        Self {
            screen_width: 80,
            screen_height: 24,
            cell_width: 1,
            cell_height: 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MouseEncoder {
    tracking_mode: ffi::GhosttyMouseTrackingMode,
    format: ffi::GhosttyMouseFormat,
    size: MouseSize,
    any_button_pressed: bool,
    track_last_cell: bool,
    last_cell: Option<(u16, u32)>,
}

impl MouseEncoder {
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            tracking_mode: ffi::GhosttyMouseTrackingMode_GHOSTTY_MOUSE_TRACKING_NONE,
            format: ffi::GhosttyMouseFormat_GHOSTTY_MOUSE_FORMAT_X10,
            size: MouseSize::default(),
            any_button_pressed: false,
            track_last_cell: false,
            last_cell: None,
        })
    }

    pub fn set_from_terminal(&mut self, terminal: &Terminal) {
        let (tracking_mode, format) = terminal.mouse_encoder_state();
        self.tracking_mode = tracking_mode;
        self.format = format;
        self.last_cell = None;
    }

    pub fn set_tracking_mode(&mut self, mode: ffi::GhosttyMouseTrackingMode) {
        if self.tracking_mode != mode {
            self.last_cell = None;
        }
        self.tracking_mode = mode;
    }

    pub fn set_size(
        &mut self,
        screen_width: u32,
        screen_height: u32,
        cell_width: u32,
        cell_height: u32,
    ) {
        if cell_width == 0 || cell_height == 0 {
            return;
        }
        self.size = MouseSize {
            screen_width,
            screen_height,
            cell_width,
            cell_height,
        };
        self.last_cell = None;
    }

    pub fn set_format(&mut self, format: ffi::GhosttyMouseFormat) {
        if self.format != format {
            self.last_cell = None;
        }
        self.format = format;
    }

    pub fn set_any_button_pressed(&mut self, pressed: bool) {
        self.any_button_pressed = pressed;
    }

    pub fn set_track_last_cell(&mut self, track: bool) {
        self.track_last_cell = track;
        if !track {
            self.last_cell = None;
        }
    }

    pub fn reset(&mut self) {
        self.last_cell = None;
    }

    pub fn encode(&mut self, event: &MouseEvent) -> Result<Vec<u8>, Error> {
        if !self.should_report(event) {
            return Ok(Vec::new());
        }

        let is_release = event.action == ffi::GhosttyMouseAction_GHOSTTY_MOUSE_ACTION_RELEASE;
        let is_motion = event.action == ffi::GhosttyMouseAction_GHOSTTY_MOUSE_ACTION_MOTION;
        if !is_release && self.pos_out_of_viewport(event) {
            let sends_motion = matches!(
                self.tracking_mode,
                ffi::GhosttyMouseTrackingMode_GHOSTTY_MOUSE_TRACKING_BUTTON
                    | ffi::GhosttyMouseTrackingMode_GHOSTTY_MOUSE_TRACKING_ANY
            );
            if !sends_motion || !self.any_button_pressed {
                return Ok(Vec::new());
            }
        }

        let cell = self.pos_to_cell(event);
        if is_motion
            && self.format != ffi::GhosttyMouseFormat_GHOSTTY_MOUSE_FORMAT_SGR_PIXELS
            && self.track_last_cell
            && self.last_cell == Some(cell)
        {
            return Ok(Vec::new());
        }
        if self.track_last_cell {
            self.last_cell = Some(cell);
        }

        let Some(button_code) = self.button_code(event) else {
            return Ok(Vec::new());
        };

        let mut out = Vec::new();
        match self.format {
            ffi::GhosttyMouseFormat_GHOSTTY_MOUSE_FORMAT_X10 => {
                if cell.0 > 222 || cell.1 > 222 {
                    return Ok(Vec::new());
                }
                out.extend_from_slice(b"\x1b[M");
                out.push(32u8.saturating_add(button_code));
                out.push(33 + cell.0 as u8);
                out.push(33 + cell.1 as u8);
            }
            ffi::GhosttyMouseFormat_GHOSTTY_MOUSE_FORMAT_UTF8 => {
                let Some(x_ch) = char::from_u32(u32::from(cell.0) + 33) else {
                    return Ok(Vec::new());
                };
                let Some(y_ch) = char::from_u32(cell.1.saturating_add(33)) else {
                    return Ok(Vec::new());
                };
                out.extend_from_slice(b"\x1b[M");
                out.push(32u8.saturating_add(button_code));
                let mut buf = [0u8; 4];
                out.extend_from_slice(x_ch.encode_utf8(&mut buf).as_bytes());
                out.extend_from_slice(y_ch.encode_utf8(&mut buf).as_bytes());
            }
            ffi::GhosttyMouseFormat_GHOSTTY_MOUSE_FORMAT_SGR => {
                let suffix = if is_release { 'm' } else { 'M' };
                let s = format!(
                    "\x1b[<{button_code};{};{}{suffix}",
                    u32::from(cell.0) + 1,
                    cell.1 + 1
                );
                out.extend_from_slice(s.as_bytes());
            }
            ffi::GhosttyMouseFormat_GHOSTTY_MOUSE_FORMAT_URXVT => {
                let s = format!(
                    "\x1b[{};{};{}M",
                    u16::from(button_code) + 32,
                    u32::from(cell.0) + 1,
                    cell.1 + 1
                );
                out.extend_from_slice(s.as_bytes());
            }
            ffi::GhosttyMouseFormat_GHOSTTY_MOUSE_FORMAT_SGR_PIXELS => {
                let px = f64::from(event.x).round() as i32;
                let py = f64::from(event.y).round() as i32;
                let suffix = if is_release { 'm' } else { 'M' };
                let s = format!("\x1b[<{button_code};{px};{py}{suffix}");
                out.extend_from_slice(s.as_bytes());
            }
            _ => {}
        }
        Ok(out)
    }

    fn should_report(&self, event: &MouseEvent) -> bool {
        match self.tracking_mode {
            ffi::GhosttyMouseTrackingMode_GHOSTTY_MOUSE_TRACKING_NONE => false,
            ffi::GhosttyMouseTrackingMode_GHOSTTY_MOUSE_TRACKING_X10 => {
                event.action == ffi::GhosttyMouseAction_GHOSTTY_MOUSE_ACTION_PRESS
                    && matches!(
                        event.button,
                        Some(
                            ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_LEFT
                                | ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_MIDDLE
                                | ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_RIGHT
                        )
                    )
            }
            ffi::GhosttyMouseTrackingMode_GHOSTTY_MOUSE_TRACKING_NORMAL => {
                event.action != ffi::GhosttyMouseAction_GHOSTTY_MOUSE_ACTION_MOTION
            }
            ffi::GhosttyMouseTrackingMode_GHOSTTY_MOUSE_TRACKING_BUTTON => event.button.is_some(),
            ffi::GhosttyMouseTrackingMode_GHOSTTY_MOUSE_TRACKING_ANY => true,
            _ => false,
        }
    }

    fn button_code(&self, event: &MouseEvent) -> Option<u8> {
        let is_release = event.action == ffi::GhosttyMouseAction_GHOSTTY_MOUSE_ACTION_RELEASE;
        let is_sgr_family = matches!(
            self.format,
            ffi::GhosttyMouseFormat_GHOSTTY_MOUSE_FORMAT_SGR
                | ffi::GhosttyMouseFormat_GHOSTTY_MOUSE_FORMAT_SGR_PIXELS
        );
        let mut acc: u8 = match event.button {
            None => 3,
            Some(_) if is_release && !is_sgr_family => 3,
            Some(ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_LEFT) => 0,
            Some(ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_MIDDLE) => 1,
            Some(ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_RIGHT) => 2,
            Some(ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_FOUR) => 64,
            Some(ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_FIVE) => 65,
            Some(ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_SIX) => 66,
            Some(ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_SEVEN) => 67,
            Some(ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_EIGHT) => 128,
            Some(ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_NINE) => 129,
            Some(_) => return None,
        };

        if self.tracking_mode != ffi::GhosttyMouseTrackingMode_GHOSTTY_MOUSE_TRACKING_X10 {
            if event.mods & (ffi::GHOSTTY_MODS_SHIFT as u16) != 0 {
                acc = acc.wrapping_add(4);
            }
            if event.mods & (ffi::GHOSTTY_MODS_ALT as u16) != 0 {
                acc = acc.wrapping_add(8);
            }
            if event.mods & (ffi::GHOSTTY_MODS_CTRL as u16) != 0 {
                acc = acc.wrapping_add(16);
            }
        }
        if event.action == ffi::GhosttyMouseAction_GHOSTTY_MOUSE_ACTION_MOTION {
            acc = acc.wrapping_add(32);
        }
        Some(acc)
    }

    fn pos_out_of_viewport(&self, event: &MouseEvent) -> bool {
        let max_x = self.size.screen_width as f32;
        let max_y = self.size.screen_height as f32;
        event.x < 0.0 || event.y < 0.0 || event.x > max_x || event.y > max_y
    }

    fn pos_to_cell(&self, event: &MouseEvent) -> (u16, u32) {
        let cols = (self.size.screen_width / self.size.cell_width.max(1)).max(1);
        let rows = (self.size.screen_height / self.size.cell_height.max(1)).max(1);
        let clamped_x = f64::from(event.x).max(0.0);
        let clamped_y = f64::from(event.y).max(0.0);
        let col = (clamped_x / f64::from(self.size.cell_width.max(1))).floor() as u32;
        let row = (clamped_y / f64::from(self.size.cell_height.max(1))).floor() as u32;
        let clamped_col = col.min(cols - 1).min(u32::from(u16::MAX)) as u16;
        let clamped_row = row.min(rows - 1);
        (clamped_col, clamped_row)
    }
}
