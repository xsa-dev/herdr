//! Incremental viewport snapshot (`RenderState`) and zero-allocation row/cell iterators.

use std::cell::Cell as StdCell;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::TermMode;

use crate::ffi;
#[cfg(windows)]
use crate::format::row_wrap_state;
use crate::format::{cell_has_text, normalized_base_char};
use crate::terminal::{cell_wide_from_flags, Terminal, TerminalInner};
use crate::{
    default_palette, ActiveScreen, CellColor, CellStyle, CellWide, CursorViewport,
    CursorVisualStyle, Dirty, Error, RenderColors, RenderCursor, RgbColor, RowSelection,
};

const POSITION_NONE: usize = usize::MAX;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellBasicData {
    pub wide: CellWide,
    pub has_hyperlink: bool,
    pub has_styling: bool,
    pub style: CellStyle,
}

impl Default for CellBasicData {
    fn default() -> Self {
        Self {
            wide: CellWide::Narrow,
            has_hyperlink: false,
            has_styling: false,
            style: CellStyle::default(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct SnapshotCell {
    base: char,
    zw_offset: u32,
    zw_len: u16,
    has_text: bool,
    wide: CellWide,
    has_hyperlink: bool,
    has_styling: bool,
    style: CellStyle,
}

impl Default for SnapshotCell {
    fn default() -> Self {
        Self {
            base: ' ',
            zw_offset: 0,
            zw_len: 0,
            has_text: false,
            wide: CellWide::Narrow,
            has_hyperlink: false,
            has_styling: false,
            style: CellStyle::default(),
        }
    }
}

#[derive(Debug, Default)]
struct SnapshotRow {
    dirty: StdCell<bool>,
    #[cfg(windows)]
    wrap_state: (bool, bool),
    cells: Vec<SnapshotCell>,
    zerowidth_pool: Vec<char>,
}

impl SnapshotRow {
    fn populate_from_grid_line(&mut self, inner: &TerminalInner, line: Line, cols: u16) {
        let cols_usize = usize::from(cols);
        self.cells.clear();
        if self.cells.capacity() < cols_usize {
            self.cells.reserve(cols_usize - self.cells.len());
        }
        self.zerowidth_pool.clear();

        #[cfg(windows)]
        {
            self.wrap_state = row_wrap_state(inner, line);
        }

        let grid = inner.term.grid();
        let topmost = inner.term.topmost_line();
        let bottommost = inner.term.bottommost_line();
        if line < topmost || line > bottommost {
            self.cells.resize(cols_usize, SnapshotCell::default());
            self.dirty.set(true);
            return;
        }

        let grid_cols = grid.columns();
        let grid_row = &grid[line];
        for col in 0..cols_usize {
            if col >= grid_cols {
                self.cells.push(SnapshotCell::default());
                continue;
            }
            let raw_cell = &grid_row[Column(col)];
            let wide = cell_wide_from_flags(raw_cell.flags);
            let has_text = cell_has_text(raw_cell);
            let base = if has_text {
                normalized_base_char(raw_cell)
            } else {
                ' '
            };

            let (zw_offset, zw_len) = if has_text {
                if let Some(zw) = raw_cell.zerowidth().filter(|s| !s.is_empty()) {
                    let offset = u32::try_from(self.zerowidth_pool.len()).unwrap_or(u32::MAX);
                    let take = zw.len().min(usize::from(u16::MAX));
                    if offset != u32::MAX {
                        self.zerowidth_pool.extend_from_slice(&zw[..take]);
                        (offset, take as u16)
                    } else {
                        (0, 0)
                    }
                } else {
                    (0, 0)
                }
            } else {
                (0, 0)
            };

            let has_hyperlink = raw_cell.hyperlink().is_some();
            let (style, has_styling) = inner.cell_style_at(raw_cell, line.0, col);

            self.cells.push(SnapshotCell {
                base,
                zw_offset,
                zw_len,
                has_text,
                wide,
                has_hyperlink,
                has_styling,
                style,
            });
        }
        self.dirty.set(true);
    }
}

pub struct RenderState {
    cols: u16,
    rows: u16,
    dirty: Dirty,
    colors: RenderColors,
    cursor: RenderCursor,
    snapshot_rows: Vec<SnapshotRow>,
    terminal_id: u64,
    last_visual_epoch: u64,
    last_screen: ActiveScreen,
    last_display_offset: usize,
}

impl RenderState {
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            cols: 0,
            rows: 0,
            dirty: Dirty::Clean,
            colors: RenderColors {
                background: RgbColor { r: 0, g: 0, b: 0 },
                foreground: RgbColor {
                    r: 255,
                    g: 255,
                    b: 255,
                },
                palette: default_palette(),
            },
            cursor: RenderCursor {
                viewport: None,
                visible: true,
                blinking: false,
                visual_style: CursorVisualStyle::Block,
            },
            snapshot_rows: Vec::new(),
            terminal_id: 0,
            last_visual_epoch: 0,
            last_screen: ActiveScreen::Primary,
            last_display_offset: 0,
        })
    }

    pub fn update(&mut self, terminal: &Terminal) -> Result<(), Error> {
        let mut inner = terminal.lock_inner();
        let new_cols = inner.logical_cols;
        let new_rows = inner.logical_rows;
        let new_screen = inner.active_screen();
        let history = inner.term.grid().history_size();
        let new_display_offset = inner.term.grid().display_offset().min(history);
        let new_colors = inner.render_colors();
        let new_cursor = compute_render_cursor(&inner);

        if self.terminal_id == inner.id
            && self.cols == new_cols
            && self.rows == new_rows
            && self.last_visual_epoch == inner.visual_epoch
            && self.last_screen == new_screen
            && self.last_display_offset == new_display_offset
            && self.colors == new_colors
            && self.cursor == new_cursor
        {
            return Ok(());
        }

        let need_full = self.terminal_id != inner.id
            || self.cols != new_cols
            || self.rows != new_rows
            || self.last_screen != new_screen
            || self.last_display_offset != new_display_offset
            || new_display_offset > 0
            || self.colors != new_colors
            || inner.force_full_damage
            || inner.last_damage_reset_epoch != self.last_visual_epoch;

        self.cols = new_cols;
        self.rows = new_rows;
        self.colors = new_colors;
        self.cursor = new_cursor;
        self.last_screen = new_screen;
        self.last_display_offset = new_display_offset;

        let rows_usize = usize::from(new_rows);
        if self.snapshot_rows.len() < rows_usize {
            self.snapshot_rows
                .resize_with(rows_usize, SnapshotRow::default);
        }

        if need_full {
            let disp_i32 = new_display_offset as i32;
            for y in 0..rows_usize {
                let line = Line((y as i32) - disp_i32);
                self.snapshot_rows[y].populate_from_grid_line(&inner, line, new_cols);
            }
            self.dirty = Dirty::Full;
        } else {
            let mut any_dirty = false;
            let disp_i32 = new_display_offset as i32;
            let limit = rows_usize.min(inner.dirty_rows.len());
            for y in 0..limit {
                if inner.dirty_rows[y] {
                    let line = Line((y as i32) - disp_i32);
                    self.snapshot_rows[y].populate_from_grid_line(&inner, line, new_cols);
                    #[cfg(windows)]
                    if y + 1 < rows_usize {
                        let next_line = Line(((y + 1) as i32) - disp_i32);
                        self.snapshot_rows[y + 1].wrap_state = row_wrap_state(&inner, next_line);
                    }
                    any_dirty = true;
                }
            }
            if any_dirty && self.dirty == Dirty::Clean {
                self.dirty = Dirty::Partial;
            }
        }

        inner.dirty_rows.fill(false);
        inner.term.reset_damage();
        inner.force_full_damage = false;
        inner.last_damage_reset_epoch = inner.visual_epoch;
        self.last_visual_epoch = inner.visual_epoch;
        self.terminal_id = inner.id;

        Ok(())
    }

    pub fn cols(&self) -> Result<u16, Error> {
        Ok(self.cols)
    }

    pub fn rows(&self) -> Result<u16, Error> {
        Ok(self.rows)
    }

    pub fn dirty(&self) -> Result<Dirty, Error> {
        Ok(self.dirty)
    }

    pub fn cursor(&self) -> Result<RenderCursor, Error> {
        Ok(self.cursor)
    }

    pub fn colors(&self) -> Result<RenderColors, Error> {
        Ok(self.colors)
    }

    pub fn clean(&mut self) -> Result<(), Error> {
        self.dirty = Dirty::Clean;
        for row in &self.snapshot_rows[..usize::from(self.rows)] {
            row.dirty.set(false);
        }
        Ok(())
    }

    pub fn set_dirty(&mut self, dirty: Dirty) -> Result<(), Error> {
        self.dirty = dirty;
        Ok(())
    }

    pub fn populate_row_iterator<'a>(
        &'a self,
        iterator: &'a mut RowIterator,
    ) -> Result<RowIter<'a>, Error> {
        iterator.y = POSITION_NONE;
        Ok(RowIter {
            iterator,
            state: self,
        })
    }
}

fn compute_render_cursor(inner: &TerminalInner) -> RenderCursor {
    let (visual_style, blinking) = inner.cursor_visual_style_and_blink();
    let visible = inner.term.mode().contains(TermMode::SHOW_CURSOR);

    let viewport = if inner.logical_cols == 0 || inner.logical_rows == 0 {
        None
    } else {
        let grid = inner.term.grid();
        let cursor_point = grid.cursor.point;
        let history = grid.history_size();
        let display_offset = grid.display_offset().min(history) as i32;
        let vp_y = cursor_point.line.0 + display_offset;
        if vp_y >= 0 && vp_y < i32::from(inner.logical_rows) {
            let x = (cursor_point.column.0 as u16).min(inner.logical_cols.saturating_sub(1));
            let col_idx = usize::from(x).min(grid.columns().saturating_sub(1));
            let wide_tail = if cursor_point.line >= inner.term.topmost_line()
                && cursor_point.line <= inner.term.bottommost_line()
                && grid.columns() > 0
            {
                cell_wide_from_flags(grid[cursor_point.line][Column(col_idx)].flags)
                    == CellWide::SpacerTail
            } else {
                false
            };
            Some(CursorViewport {
                x,
                y: vp_y as u16,
                wide_tail,
            })
        } else {
            None
        }
    };

    RenderCursor {
        viewport,
        visible,
        blinking,
        visual_style,
    }
}

pub struct RowIterator {
    y: usize,
}

impl RowIterator {
    pub fn new() -> Result<Self, Error> {
        Ok(Self { y: POSITION_NONE })
    }
}

pub struct RowIter<'a> {
    iterator: &'a mut RowIterator,
    state: &'a RenderState,
}

impl RowIter<'_> {
    #[inline]
    fn row_count(&self) -> usize {
        usize::from(self.state.rows)
    }

    #[inline]
    fn current_row(&self) -> Result<&SnapshotRow, Error> {
        let count = self.row_count();
        if self.iterator.y >= count {
            return Err(Error(ffi::GhosttyResult_GHOSTTY_INVALID_VALUE));
        }
        self.state
            .snapshot_rows
            .get(self.iterator.y)
            .ok_or(Error(ffi::GhosttyResult_GHOSTTY_INVALID_VALUE))
    }

    // Preserves the existing C-style cursor step method consumed across `src/pane/terminal.rs`.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> bool {
        let next_y = self.iterator.y.wrapping_add(1);
        if next_y >= self.row_count() {
            return false;
        }
        self.iterator.y = next_y;
        true
    }

    pub fn next_dirty(&mut self) -> Option<u16> {
        let count = self.row_count();
        match self.state.dirty {
            Dirty::Clean => None,
            Dirty::Full => {
                let next_y = self.iterator.y.wrapping_add(1);
                if next_y >= count {
                    return None;
                }
                self.iterator.y = next_y;
                Some(next_y as u16)
            }
            Dirty::Partial => {
                let mut next_y = self.iterator.y.wrapping_add(1);
                while next_y < count {
                    if self.state.snapshot_rows[next_y].dirty.get() {
                        self.iterator.y = next_y;
                        return Some(next_y as u16);
                    }
                    next_y += 1;
                }
                None
            }
        }
    }

    pub fn dirty(&self) -> Result<bool, Error> {
        Ok(self.current_row()?.dirty.get())
    }

    #[cfg(windows)]
    pub fn wrap_state(&self) -> Result<(bool, bool), Error> {
        Ok(self.current_row()?.wrap_state)
    }

    pub fn clear_dirty(&mut self) -> Result<(), Error> {
        self.set_dirty(false)
    }

    pub fn set_dirty(&mut self, dirty: bool) -> Result<(), Error> {
        self.current_row()?.dirty.set(dirty);
        Ok(())
    }

    pub fn selection(&self) -> Result<Option<RowSelection>, Error> {
        let _ = self.current_row()?;
        Ok(None)
    }

    pub fn populate_cells<'b>(
        &'b mut self,
        cells: &'b mut RowCells,
    ) -> Result<RowCellIter<'b>, Error> {
        let count = self.row_count();
        if self.iterator.y >= count {
            return Err(Error(ffi::GhosttyResult_GHOSTTY_INVALID_VALUE));
        }
        let row = self
            .state
            .snapshot_rows
            .get(self.iterator.y)
            .ok_or(Error(ffi::GhosttyResult_GHOSTTY_INVALID_VALUE))?;
        cells.x = POSITION_NONE;
        Ok(RowCellIter {
            cells,
            row,
            palette: &self.state.colors.palette,
        })
    }
}

pub struct RowCells {
    x: usize,
}

impl RowCells {
    pub fn new() -> Result<Self, Error> {
        Ok(Self { x: POSITION_NONE })
    }
}

pub struct RowCellIter<'a> {
    cells: &'a mut RowCells,
    row: &'a SnapshotRow,
    palette: &'a [RgbColor; 256],
}

impl RowCellIter<'_> {
    #[inline]
    fn current_cell(&self) -> Result<&SnapshotCell, Error> {
        self.row
            .cells
            .get(self.cells.x)
            .ok_or(Error(ffi::GhosttyResult_GHOSTTY_INVALID_VALUE))
    }

    // Preserves the existing C-style cursor step method consumed across `src/pane/terminal.rs`.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> bool {
        let next_x = self.cells.x.wrapping_add(1);
        if next_x >= self.row.cells.len() {
            return false;
        }
        self.cells.x = next_x;
        true
    }

    pub fn select(&mut self, x: u16) -> Result<(), Error> {
        let idx = usize::from(x);
        if idx >= self.row.cells.len() {
            return Err(Error(ffi::GhosttyResult_GHOSTTY_INVALID_VALUE));
        }
        self.cells.x = idx;
        Ok(())
    }

    pub fn is_default_blank(&self) -> Result<bool, Error> {
        let cell = self.current_cell()?;
        Ok(!cell.has_text
            && cell.wide == CellWide::Narrow
            && !cell.has_hyperlink
            && !cell.has_styling)
    }

    pub fn basic_data(&self) -> Result<CellBasicData, Error> {
        let cell = self.current_cell()?;
        Ok(CellBasicData {
            wide: cell.wide,
            has_hyperlink: cell.has_hyperlink,
            has_styling: cell.has_styling,
            style: cell.style,
        })
    }

    pub fn wide(&self) -> Result<CellWide, Error> {
        Ok(self.current_cell()?.wide)
    }

    pub fn has_hyperlink(&self) -> Result<bool, Error> {
        Ok(self.current_cell()?.has_hyperlink)
    }

    pub fn style(&self) -> Result<CellStyle, Error> {
        Ok(self.current_cell()?.style)
    }

    pub fn content_bg_color(&self) -> Result<Option<CellColor>, Error> {
        let cell = self.current_cell()?;
        if !cell.has_text {
            Ok(cell.style.bg_color)
        } else {
            Ok(None)
        }
    }

    pub fn fg_color(&self) -> Result<Option<RgbColor>, Error> {
        let Ok(cell) = self.current_cell() else {
            return Ok(None);
        };
        Ok(match cell.style.fg_color {
            Some(CellColor::Palette(idx)) => Some(self.palette[usize::from(idx)]),
            Some(CellColor::Rgb(rgb)) => Some(rgb),
            None => None,
        })
    }

    pub fn bg_color(&self) -> Result<Option<RgbColor>, Error> {
        let Ok(cell) = self.current_cell() else {
            return Ok(None);
        };
        Ok(match cell.style.bg_color {
            Some(CellColor::Palette(idx)) => Some(self.palette[usize::from(idx)]),
            Some(CellColor::Rgb(rgb)) => Some(rgb),
            None => None,
        })
    }

    pub fn grapheme_text(&self) -> Result<String, Error> {
        let mut bytes = Vec::new();
        let mut text = String::new();
        self.grapheme_text_into(&mut bytes, &mut text)?;
        Ok(text)
    }

    pub fn grapheme_text_into(&self, bytes: &mut Vec<u8>, text: &mut String) -> Result<(), Error> {
        text.clear();
        bytes.clear();

        let cell = self.current_cell()?;
        if !cell.has_text {
            return Ok(());
        }
        text.push(cell.base);
        if cell.zw_len > 0 {
            let start = cell.zw_offset as usize;
            let end = start.saturating_add(usize::from(cell.zw_len));
            if let Some(slice) = self.row.zerowidth_pool.get(start..end) {
                for &ch in slice {
                    text.push(ch);
                }
            }
        }
        Ok(())
    }
}

const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<RenderState>();
    assert_send::<RowIterator>();
    assert_send::<RowCells>();
};
