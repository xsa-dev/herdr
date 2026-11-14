//! Cell/row readers, plain/VT selection formatters, `clear_screen`, and bounded link resolution.

use std::fmt::Write as _;

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::vte::ansi::{ClearMode, Handler};

use crate::ffi;
use crate::terminal::{cell_wide_from_flags, Terminal, TerminalInner, FLAG_HAS_TEXT};
use crate::{
    CellColor, CellStyle, CellWide, Error, LinkRegion, LinkTarget, ScreenTextCell, ScreenTextRow,
};

const LINK_SCAN_MAX_CELLS: usize = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EmitFormat {
    Plain,
    Vt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GridPos {
    line: i32,
    col: u16,
}

type LinkSelection = (LinkTarget, Option<(GridPos, GridPos)>);

#[inline]
pub(crate) fn cell_has_text(cell: &Cell) -> bool {
    let wide = cell_wide_from_flags(cell.flags);
    if matches!(wide, CellWide::SpacerTail | CellWide::SpacerHead) {
        return false;
    }
    cell.flags.contains(FLAG_HAS_TEXT) || (cell.c != ' ' && cell.c != '\t')
}

#[inline]
pub(crate) fn normalized_base_char(cell: &Cell) -> char {
    if cell.c == '\t' {
        ' '
    } else {
        cell.c
    }
}

#[inline]
pub(crate) fn row_is_soft_wrapped(inner: &TerminalInner, line: Line) -> bool {
    let grid = inner.term.grid();
    if line < inner.term.topmost_line() || line > inner.term.bottommost_line() {
        return false;
    }
    let check_col = usize::from(inner.logical_cols)
        .saturating_sub(1)
        .min(grid.columns().saturating_sub(1));
    grid[line][Column(check_col)]
        .flags
        .contains(Flags::WRAPLINE)
}

#[inline]
pub(crate) fn row_wrap_state(inner: &TerminalInner, line: Line) -> (bool, bool) {
    let soft_wrapped = row_is_soft_wrapped(inner, line);
    let wrap_continuation =
        line > inner.term.topmost_line() && row_is_soft_wrapped(inner, line - 1i32);
    (soft_wrapped, wrap_continuation)
}

#[inline]
fn cell_graphemes_owned(cell: &Cell) -> Vec<u32> {
    if !cell_has_text(cell) {
        return Vec::new();
    }
    let base = u32::from(normalized_base_char(cell));
    let zw = cell.zerowidth().unwrap_or(&[]);
    let mut out = Vec::with_capacity(1 + zw.len());
    out.push(base);
    for &ch in zw {
        out.push(u32::from(ch));
    }
    out
}

#[inline]
fn cell_utf8_len(cell: &Cell) -> usize {
    if !cell_has_text(cell) {
        return 0;
    }
    let mut len = normalized_base_char(cell).len_utf8();
    if let Some(zw) = cell.zerowidth() {
        for &ch in zw {
            len += ch.len_utf8();
        }
    }
    len
}

impl TerminalInner {
    fn resolve_screen_point(&self, x: u16, y: u32) -> Result<GridPos, Error> {
        if x >= self.logical_cols {
            return Err(Error(ffi::GhosttyResult_GHOSTTY_INVALID_VALUE));
        }
        let history = self.term.grid().history_size();
        let total = history + usize::from(self.logical_rows);
        let y_usize =
            usize::try_from(y).map_err(|_| Error(ffi::GhosttyResult_GHOSTTY_INVALID_VALUE))?;
        if y_usize >= total {
            return Err(Error(ffi::GhosttyResult_GHOSTTY_INVALID_VALUE));
        }
        let line = (y_usize as i32) - (history as i32);
        Ok(GridPos { line, col: x })
    }

    fn resolve_viewport_point(&self, x: u16, y: u32) -> Result<GridPos, Error> {
        if x >= self.logical_cols || y >= u32::from(self.logical_rows) {
            return Err(Error(ffi::GhosttyResult_GHOSTTY_INVALID_VALUE));
        }
        let history = self.term.grid().history_size();
        let display_offset = self.term.grid().display_offset().min(history) as i32;
        let line = (y as i32) - display_offset;
        Ok(GridPos { line, col: x })
    }

    fn grid_pos_to_viewport_clipped(&self, pos: GridPos, clipped: (u16, u32)) -> (u16, u32) {
        let history = self.term.grid().history_size();
        let display_offset = self.term.grid().display_offset().min(history) as i32;
        let vp_y = pos.line + display_offset;
        if vp_y < 0 || vp_y >= i32::from(self.logical_rows) {
            return clipped;
        }
        (
            pos.col.min(self.logical_cols.saturating_sub(1)),
            vp_y as u32,
        )
    }

    fn format_grid_range(
        &self,
        raw_start: GridPos,
        raw_end: GridPos,
        rectangle: bool,
        emit: EmitFormat,
        unwrap: bool,
        trim: bool,
    ) -> String {
        let cols = self.logical_cols;
        if cols == 0 || self.logical_rows == 0 {
            return String::new();
        }
        let topmost = self.term.topmost_line().0;
        let bottommost = self.term.bottommost_line().0;

        let (start_x, start_y, mut end_x, mut end_y) = if rectangle {
            (
                raw_start.col.min(raw_end.col),
                raw_start.line.min(raw_end.line),
                raw_start.col.max(raw_end.col),
                raw_start.line.max(raw_end.line),
            )
        } else if (raw_start.line, raw_start.col) <= (raw_end.line, raw_end.col) {
            (raw_start.col, raw_start.line, raw_end.col, raw_end.line)
        } else {
            (raw_end.col, raw_end.line, raw_start.col, raw_start.line)
        };

        if start_x >= cols || start_y > bottommost || end_y < topmost || start_y > end_y {
            return String::new();
        }
        let start_y = start_y.max(topmost);
        end_y = end_y.min(bottommost);
        end_x = end_x.min(cols - 1);

        // Match Ghostty formatter: if end falls on a spacer_head while unwrapping, move to start of next row.
        if unwrap && !rectangle {
            let end_cell = &self.term.grid()[Line(end_y)][Column(usize::from(end_x))];
            if cell_wide_from_flags(end_cell.flags) == CellWide::SpacerHead && end_y < bottommost {
                end_y += 1;
                end_x = 0;
            }
        }

        if start_y == end_y && start_x > end_x {
            return String::new();
        }

        let mut out = String::new();
        let mut blank_rows: usize = 0;
        let mut blank_cells: usize = 0;
        let mut active_style = CellStyle::default();
        let mut active_hyperlink: Option<String> = None;
        let default_style = CellStyle::default();

        for line_idx in start_y..=end_y {
            let line = Line(line_idx);
            let row = &self.term.grid()[line];

            let row_end_x = if rectangle || line_idx == end_y {
                usize::from(end_x) + 1
            } else {
                usize::from(cols)
            };

            let row_start_x = if start_x > 0 && (rectangle || line_idx == start_y) {
                let first_wide = cell_wide_from_flags(row[Column(usize::from(start_x))].flags);
                match first_wide {
                    CellWide::SpacerTail => usize::from(start_x) - 1,
                    CellWide::SpacerHead => continue,
                    CellWide::Narrow | CellWide::Wide => usize::from(start_x),
                }
            } else {
                0
            };

            if row_start_x >= row_end_x {
                blank_rows += 1;
                continue;
            }

            // Check whether any cell in subset has printable content (or non-default styling in VT mode).
            let mut row_has_content = false;
            for col_idx in row_start_x..row_end_x {
                let cell = &row[Column(col_idx)];
                if cell_has_text(cell) {
                    row_has_content = true;
                    break;
                }
                if emit == EmitFormat::Vt {
                    let (_, has_styling) = self.cell_style_at(cell, line_idx, col_idx);
                    if has_styling || cell.hyperlink().is_some() {
                        row_has_content = true;
                        break;
                    }
                }
            }

            if !row_has_content {
                blank_rows += 1;
                continue;
            }

            if blank_rows > 0 {
                if emit == EmitFormat::Vt {
                    if active_hyperlink.take().is_some() {
                        out.push_str("\x1b]8;;\x1b\\");
                    }
                    if active_style != default_style {
                        out.push_str("\x1b[0m");
                        active_style = default_style;
                    }
                }
                let sep = match emit {
                    EmitFormat::Plain => "\n",
                    EmitFormat::Vt => "\r\n",
                };
                for _ in 0..blank_rows {
                    out.push_str(sep);
                }
                blank_rows = 0;
            }

            let (soft_wrapped, wrap_continuation) = row_wrap_state(self, line);
            if !soft_wrapped || !unwrap {
                blank_rows += 1;
            }
            if !wrap_continuation || !unwrap {
                blank_cells = 0;
            }

            for col_idx in row_start_x..row_end_x {
                let cell = &row[Column(col_idx)];
                let wide = cell_wide_from_flags(cell.flags);
                if matches!(wide, CellWide::SpacerHead | CellWide::SpacerTail) {
                    continue;
                }

                let has_text = cell_has_text(cell);
                let (cell_style, has_styling, cell_hyperlink) = if emit == EmitFormat::Vt {
                    let (st, hs) = self.cell_style_at(cell, line_idx, col_idx);
                    (st, hs, cell.hyperlink())
                } else {
                    (default_style, false, None)
                };

                let is_blank_cell =
                    if emit == EmitFormat::Vt && (has_styling || cell_hyperlink.is_some()) {
                        false
                    } else if !has_text {
                        true
                    } else {
                        let base = normalized_base_char(cell);
                        let has_zw = cell.zerowidth().is_some_and(|zw| !zw.is_empty());
                        base == ' ' && !has_zw && trim
                    };

                if is_blank_cell {
                    blank_cells += 1;
                    continue;
                }

                if blank_cells > 0 {
                    if emit == EmitFormat::Vt {
                        if active_hyperlink.take().is_some() {
                            out.push_str("\x1b]8;;\x1b\\");
                        }
                        if active_style != default_style {
                            out.push_str("\x1b[0m");
                            active_style = default_style;
                        }
                    }
                    for _ in 0..blank_cells {
                        out.push(' ');
                    }
                    blank_cells = 0;
                }

                if emit == EmitFormat::Vt {
                    if cell_style != active_style {
                        if cell_style == default_style {
                            out.push_str("\x1b[0m");
                        } else {
                            write_vt_sgr(&mut out, &cell_style);
                        }
                        active_style = cell_style;
                    }
                    let next_uri = cell_hyperlink.as_ref().map(|h| h.uri());
                    if next_uri != active_hyperlink.as_deref() {
                        match next_uri {
                            Some(uri) => {
                                let _ = write!(out, "\x1b]8;;{uri}\x1b\\");
                                active_hyperlink = Some(uri.to_owned());
                            }
                            None => {
                                out.push_str("\x1b]8;;\x1b\\");
                                active_hyperlink = None;
                            }
                        }
                    }
                }

                if has_text {
                    out.push(normalized_base_char(cell));
                    if let Some(zw) = cell.zerowidth() {
                        for &ch in zw {
                            out.push(ch);
                        }
                    }
                } else {
                    out.push(' ');
                }
            }
        }

        if emit == EmitFormat::Vt {
            if active_hyperlink.is_some() {
                out.push_str("\x1b]8;;\x1b\\");
            }
            if active_style != default_style {
                out.push_str("\x1b[0m");
            }
        }

        out
    }

    fn select_non_whitespace_word_bounded(&self, origin: GridPos) -> Option<(GridPos, GridPos)> {
        if LINK_SCAN_MAX_CELLS == 0 {
            return None;
        }
        let mut remaining = LINK_SCAN_MAX_CELLS - 1;
        let cols = self.logical_cols;
        if cols == 0 {
            return None;
        }
        let topmost = self.term.topmost_line().0;
        let bottommost = self.term.bottommost_line().0;
        if origin.line < topmost || origin.line > bottommost || origin.col >= cols {
            return None;
        }

        let start_cell = &self.term.grid()[Line(origin.line)][Column(usize::from(origin.col))];
        if !cell_has_text(start_cell) || normalized_base_char(start_cell).is_whitespace() {
            return None;
        }

        // Scan forward across soft-wrapped rows.
        let mut end = origin;
        let mut cur = origin;
        loop {
            let next = if cur.col + 1 < cols {
                GridPos {
                    line: cur.line,
                    col: cur.col + 1,
                }
            } else if cur.line < bottommost && row_is_soft_wrapped(self, Line(cur.line)) {
                GridPos {
                    line: cur.line + 1,
                    col: 0,
                }
            } else {
                break;
            };

            if remaining == 0 {
                return None;
            }
            remaining -= 1;

            let cell = &self.term.grid()[Line(next.line)][Column(usize::from(next.col))];
            let wide = cell_wide_from_flags(cell.flags);
            if matches!(wide, CellWide::SpacerTail | CellWide::SpacerHead) {
                end = next;
                cur = next;
                continue;
            }
            if !cell_has_text(cell) || normalized_base_char(cell).is_whitespace() {
                break;
            }
            end = next;
            cur = next;
        }

        // Scan backward across soft-wrapped rows.
        let mut start = origin;
        let mut cur = origin;
        loop {
            let prev = if cur.col > 0 {
                GridPos {
                    line: cur.line,
                    col: cur.col - 1,
                }
            } else if cur.line > topmost && row_is_soft_wrapped(self, Line(cur.line - 1)) {
                GridPos {
                    line: cur.line - 1,
                    col: cols - 1,
                }
            } else {
                break;
            };

            if remaining == 0 {
                return None;
            }
            remaining -= 1;

            let cell = &self.term.grid()[Line(prev.line)][Column(usize::from(prev.col))];
            let wide = cell_wide_from_flags(cell.flags);
            if matches!(wide, CellWide::SpacerTail | CellWide::SpacerHead) {
                start = prev;
                cur = prev;
                continue;
            }
            if !cell_has_text(cell) || normalized_base_char(cell).is_whitespace() {
                break;
            }
            start = prev;
            cur = prev;
        }

        Some((start, end))
    }

    fn viewport_link_selection(&self, x: u16, y: u32) -> Result<Option<LinkSelection>, Error> {
        let mut clicked = self.resolve_viewport_point(x, y)?;
        {
            let cell = &self.term.grid()[Line(clicked.line)][Column(usize::from(clicked.col))];
            if cell_wide_from_flags(cell.flags) == CellWide::SpacerTail {
                clicked.col = clicked.col.saturating_sub(1);
            }
        }
        let clicked_cell = &self.term.grid()[Line(clicked.line)][Column(usize::from(clicked.col))];
        if let Some(link) = clicked_cell.hyperlink() {
            return Ok(Some((LinkTarget::Uri(link.uri().to_owned()), None)));
        }
        if !cell_has_text(clicked_cell) || normalized_base_char(clicked_cell).is_whitespace() {
            return Ok(None);
        }

        let Some((sel_start, sel_end)) = self.select_non_whitespace_word_bounded(clicked) else {
            return Ok(None);
        };

        let text =
            self.format_grid_range(sel_start, sel_end, false, EmitFormat::Plain, true, false);
        let prefix =
            self.format_grid_range(sel_start, clicked, false, EmitFormat::Plain, true, false);
        let clicked_len = cell_utf8_len(clicked_cell);

        Ok(Some((
            LinkTarget::Text {
                text,
                clicked_byte: prefix.len().saturating_sub(clicked_len),
            },
            Some((sel_start, sel_end)),
        )))
    }
}

fn write_vt_sgr(out: &mut String, style: &CellStyle) {
    out.push_str("\x1b[0");
    if style.bold {
        out.push_str(";1");
    }
    if style.faint {
        out.push_str(";2");
    }
    if style.italic {
        out.push_str(";3");
    }
    match style.underline {
        1 => out.push_str(";4"),
        2 => out.push_str(";4:2"),
        3 => out.push_str(";4:3"),
        4 => out.push_str(";4:4"),
        5 => out.push_str(";4:5"),
        _ => {}
    }
    if style.blink {
        out.push_str(";5");
    }
    if style.inverse {
        out.push_str(";7");
    }
    if style.invisible {
        out.push_str(";8");
    }
    if style.strikethrough {
        out.push_str(";9");
    }
    if style.overline {
        out.push_str(";53");
    }
    if let Some(fg) = style.fg_color {
        match fg {
            CellColor::Palette(idx @ 0..=7) => {
                let _ = write!(out, ";{}", 30 + idx);
            }
            CellColor::Palette(idx @ 8..=15) => {
                let _ = write!(out, ";{}", 90 + (idx - 8));
            }
            CellColor::Palette(idx) => {
                let _ = write!(out, ";38;5;{idx}");
            }
            CellColor::Rgb(rgb) => {
                let _ = write!(out, ";38;2;{};{};{}", rgb.r, rgb.g, rgb.b);
            }
        }
    }
    if let Some(bg) = style.bg_color {
        match bg {
            CellColor::Palette(idx @ 0..=7) => {
                let _ = write!(out, ";{}", 40 + idx);
            }
            CellColor::Palette(idx @ 8..=15) => {
                let _ = write!(out, ";{}", 100 + (idx - 8));
            }
            CellColor::Palette(idx) => {
                let _ = write!(out, ";48;5;{idx}");
            }
            CellColor::Rgb(rgb) => {
                let _ = write!(out, ";48;2;{};{};{}", rgb.r, rgb.g, rgb.b);
            }
        }
    }
    if let Some(ul) = style.underline_color {
        match ul {
            CellColor::Palette(idx) => {
                let _ = write!(out, ";58;5;{idx}");
            }
            CellColor::Rgb(rgb) => {
                let _ = write!(out, ";58;2;{};{};{}", rgb.r, rgb.g, rgb.b);
            }
        }
    }
    out.push('m');
}

impl Terminal {
    pub fn screen_cell(&self, x: u16, y: u32) -> Result<(CellWide, Vec<u32>), Error> {
        let inner = self.lock_inner();
        let pos = inner.resolve_screen_point(x, y)?;
        let cell = &inner.term.grid()[Line(pos.line)][Column(usize::from(pos.col))];
        Ok((cell_wide_from_flags(cell.flags), cell_graphemes_owned(cell)))
    }

    pub fn screen_text_rows(&self) -> Result<Vec<ScreenTextRow>, Error> {
        self.screen_text_rows_range(0, usize::MAX)
    }

    /// Visit a screen row without allocating an owned grapheme vector for each cell.
    /// The borrowed graphemes are valid only for the duration of each callback.
    /// Like `screen_text_rows_range`, rows outside the active screen/history are empty.
    pub fn for_each_screen_row_cell(
        &self,
        row: u32,
        mut visit: impl FnMut(CellWide, &[u32]),
    ) -> Result<(), Error> {
        let inner = self.lock_inner();
        let history = inner.term.grid().history_size();
        let total_rows = history + usize::from(inner.logical_rows);
        let Ok(row_usize) = usize::try_from(row) else {
            return Ok(());
        };
        if row_usize >= total_rows {
            return Ok(());
        }
        let line = Line((row_usize as i32) - (history as i32));
        let cols = usize::from(inner.logical_cols);
        let grid_row = &inner.term.grid()[line];
        let mut scratch: Vec<u32> = Vec::new();

        for x in 0..cols {
            let cell = &grid_row[Column(x)];
            let wide = cell_wide_from_flags(cell.flags);
            if !cell_has_text(cell) {
                visit(wide, &[]);
                continue;
            }
            let base = u32::from(normalized_base_char(cell));
            match cell.zerowidth() {
                None | Some([]) => {
                    let single = [base];
                    visit(wide, &single);
                }
                Some(zw) => {
                    scratch.clear();
                    scratch.reserve(1 + zw.len());
                    scratch.push(base);
                    for &ch in zw {
                        scratch.push(u32::from(ch));
                    }
                    visit(wide, &scratch);
                }
            }
        }
        Ok(())
    }

    pub fn screen_text_rows_range(
        &self,
        start_row: usize,
        end_row_exclusive: usize,
    ) -> Result<Vec<ScreenTextRow>, Error> {
        let inner = self.lock_inner();
        let history = inner.term.grid().history_size();
        let total_rows = history + usize::from(inner.logical_rows);
        let start_row = start_row.min(total_rows);
        let end_row_exclusive = end_row_exclusive.min(total_rows).max(start_row);
        let cols = usize::from(inner.logical_cols);

        let mut rows = Vec::with_capacity(end_row_exclusive.saturating_sub(start_row));
        for y in start_row..end_row_exclusive {
            let line = Line((y as i32) - (history as i32));
            let (soft_wrapped, wrap_continuation) = row_wrap_state(&inner, line);
            let grid_row = &inner.term.grid()[line];
            let mut cells = Vec::with_capacity(cols);
            for x in 0..cols {
                let cell = &grid_row[Column(x)];
                cells.push(ScreenTextCell {
                    wide: cell_wide_from_flags(cell.flags),
                    graphemes: cell_graphemes_owned(cell),
                });
            }
            rows.push(ScreenTextRow {
                cells,
                soft_wrapped,
                wrap_continuation,
            });
        }
        Ok(rows)
    }

    pub fn viewport_hyperlink_uri(&self, x: u16, y: u32) -> Result<Option<String>, Error> {
        let inner = self.lock_inner();
        let pos = inner.resolve_viewport_point(x, y)?;
        let cell = &inner.term.grid()[Line(pos.line)][Column(usize::from(pos.col))];
        Ok(cell.hyperlink().map(|h| h.uri().to_owned()))
    }

    pub fn viewport_link_target(&self, x: u16, y: u32) -> Result<Option<LinkTarget>, Error> {
        let inner = self.lock_inner();
        Ok(inner
            .viewport_link_selection(x, y)?
            .map(|(target, _)| target))
    }

    /// Resolve only the bounded plain-text token. OSC 8 regions are resolved by
    /// clients from frame hyperlink IDs; their full URI activation path is unchanged.
    pub fn viewport_link_regions(
        &self,
        x: u16,
        y: u32,
        resolve: fn(&str, usize) -> Option<std::ops::Range<usize>>,
    ) -> Result<Vec<LinkRegion>, Error> {
        let inner = self.lock_inner();
        let cols = inner.logical_cols;
        let rows = inner.logical_rows;
        if x >= cols || y >= u32::from(rows) {
            return Ok(Vec::new());
        }
        let Some((LinkTarget::Text { text, clicked_byte }, Some((sel_start, sel_end)))) =
            inner.viewport_link_selection(x, y)?
        else {
            return Ok(Vec::new());
        };
        let Some(range) = resolve(&text, clicked_byte) else {
            return Ok(Vec::new());
        };

        let (start_col, start_row) = inner.grid_pos_to_viewport_clipped(sel_start, (0, 0));
        let (end_col, end_row) =
            inner.grid_pos_to_viewport_clipped(sel_end, (cols - 1, u32::from(rows - 1)));

        let first = inner.resolve_viewport_point(start_col, start_row)?;
        let first_cell = &inner.term.grid()[Line(first.line)][Column(usize::from(first.col))];
        let prefix =
            inner.format_grid_range(sel_start, first, false, EmitFormat::Plain, true, false);
        let mut byte = prefix.len().saturating_sub(cell_utf8_len(first_cell));
        let mut regions: Vec<LinkRegion> = Vec::new();

        for row in start_row..=end_row {
            let line_pos = inner.resolve_viewport_point(0, row)?;
            let grid_row = &inner.term.grid()[Line(line_pos.line)];
            let left = if row == start_row { start_col } else { 0 };
            let right = if row == end_row { end_col } else { cols - 1 };
            for col in left..=right {
                let cell = &grid_row[Column(usize::from(col))];
                let wide = cell_wide_from_flags(cell.flags);
                if matches!(wide, CellWide::SpacerHead | CellWide::SpacerTail) {
                    continue;
                }
                let len = cell_utf8_len(cell);
                if byte < range.end && byte + len > range.start {
                    let end = if wide == CellWide::Wide {
                        (col + 1).min(cols - 1)
                    } else {
                        col
                    };
                    if let Some(last) = regions
                        .last_mut()
                        .filter(|last| u32::from(last.row) == row && last.end_col + 1 == col)
                    {
                        last.end_col = end;
                    } else {
                        regions.push(LinkRegion {
                            row: row as u16,
                            start_col: col,
                            end_col: end,
                        });
                    }
                }
                byte += len;
            }
        }
        Ok(regions)
    }

    pub fn read_text_viewport(
        &self,
        start: (u16, u32),
        end: (u16, u32),
        rectangle: bool,
    ) -> Result<String, Error> {
        let inner = self.lock_inner();
        let s = inner.resolve_viewport_point(start.0, start.1)?;
        let e = inner.resolve_viewport_point(end.0, end.1)?;
        Ok(inner.format_grid_range(s, e, rectangle, EmitFormat::Plain, true, true))
    }

    pub fn read_ansi_viewport(
        &self,
        start: (u16, u32),
        end: (u16, u32),
        rectangle: bool,
    ) -> Result<String, Error> {
        let inner = self.lock_inner();
        let s = inner.resolve_viewport_point(start.0, start.1)?;
        let e = inner.resolve_viewport_point(end.0, end.1)?;
        Ok(inner.format_grid_range(s, e, rectangle, EmitFormat::Vt, false, true))
    }

    pub fn read_text_screen(
        &self,
        start: (u16, u32),
        end: (u16, u32),
        rectangle: bool,
    ) -> Result<String, Error> {
        let inner = self.lock_inner();
        let s = inner.resolve_screen_point(start.0, start.1)?;
        let e = inner.resolve_screen_point(end.0, end.1)?;
        Ok(inner.format_grid_range(s, e, rectangle, EmitFormat::Plain, true, true))
    }

    pub fn read_ansi_screen(
        &self,
        start: (u16, u32),
        end: (u16, u32),
        rectangle: bool,
        unwrap: bool,
    ) -> Result<String, Error> {
        let inner = self.lock_inner();
        let s = inner.resolve_screen_point(start.0, start.1)?;
        let e = inner.resolve_screen_point(end.0, end.1)?;
        Ok(inner.format_grid_range(s, e, rectangle, EmitFormat::Vt, unwrap, true))
    }

    pub fn clear_screen(&mut self) -> bool {
        let inner = self.inner_mut();
        if inner.is_alt_screen() {
            return false;
        }
        let rows = i32::from(inner.logical_rows);
        let grid_cols = inner.term.columns();
        let cursor_y = inner
            .term
            .grid()
            .cursor
            .point
            .line
            .0
            .clamp(0, rows.saturating_sub(1));

        // Walk upward within active viewport while rows continue a soft wrap.
        let mut first = cursor_y;
        while first > 0 {
            let (_, wrap_cont) = row_wrap_state(inner, Line(first));
            if !wrap_cont {
                break;
            }
            first -= 1;
        }

        // Walk downward within active viewport while rows soft-wrap to the next line.
        let mut last = cursor_y;
        while last + 1 < rows {
            if !row_is_soft_wrapped(inner, Line(last)) {
                break;
            }
            last += 1;
        }

        // Snapshot retained active-line cells before erasing history/viewport.
        let kept_len = (last - first + 1).max(0) as usize;
        let mut kept_rows: Vec<Vec<Cell>> = Vec::with_capacity(kept_len);
        for line_idx in first..=last {
            let grid_row = &inner.term.grid()[Line(line_idx)];
            let mut row_cells = Vec::with_capacity(grid_cols);
            for col_idx in 0..grid_cols {
                row_cells.push(grid_row[Column(col_idx)].clone());
            }
            kept_rows.push(row_cells);
        }

        // Erase scrollback history and scroll active viewport to bottom.
        Handler::clear_screen(&mut inner.term, ClearMode::Saved);
        inner.term.scroll_display(Scroll::Bottom);

        // Write retained soft-wrapped line at top of viewport and clear remaining rows.
        for (dst_idx, src_cells) in kept_rows.into_iter().enumerate() {
            let dst_row = &mut inner.term.grid_mut()[Line(dst_idx as i32)];
            for (col_idx, cell) in src_cells.into_iter().enumerate() {
                dst_row[Column(col_idx)] = cell;
            }
        }
        for dst_idx in (kept_len as i32)..rows {
            let dst_row = &mut inner.term.grid_mut()[Line(dst_idx)];
            for col_idx in 0..grid_cols {
                dst_row[Column(col_idx)] = Cell::default();
            }
        }

        // Shift cursor and sparse blink/overline coordinates by `-first`.
        inner.term.grid_mut().cursor.point.line = Line(cursor_y - first);
        if !inner.primary_blink_cells.is_empty() {
            inner.primary_blink_cells = inner
                .primary_blink_cells
                .drain()
                .filter_map(|(l, c)| (l >= first && l <= last).then_some((l - first, c)))
                .collect();
        }
        if !inner.primary_overline_cells.is_empty() {
            inner.primary_overline_cells = inner
                .primary_overline_cells
                .drain()
                .filter_map(|(l, c)| (l >= first && l <= last).then_some((l - first, c)))
                .collect();
        }
        inner.last_printed = None;
        #[cfg(windows)]
        {
            inner.tracked_grid_line = None;
        }
        inner.mark_full_damage();
        true
    }
}
