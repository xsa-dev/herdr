use ghostty_vt::{CellWide, Terminal};

pub fn text(output: &[u8], cols: u16, rows: u16) -> String {
    let mut terminal = Terminal::new(cols, rows, 0).expect("valid terminal dimensions");
    terminal.write(output);
    let screen_rows = terminal.screen_text_rows().expect("screen_text_rows");

    let mut lines: Vec<String> = Vec::with_capacity(screen_rows.len());
    for row in &screen_rows {
        let mut line = String::new();
        for cell in &row.cells {
            if matches!(cell.wide, CellWide::SpacerTail | CellWide::SpacerHead) {
                continue;
            }
            if cell.graphemes.is_empty() {
                line.push(' ');
            } else {
                for &cp in &cell.graphemes {
                    if let Some(ch) = char::from_u32(cp) {
                        line.push(ch);
                    }
                }
            }
        }
        lines.push(line.trim_end_matches(' ').to_owned());
    }

    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines.join("\n")
}

#[test]
fn screen_text_reconstructs_partial_redraws() {
    let output = b"REMOTE_SURVIVED\x1b[1;8HSTILL_SELECTED";
    assert!(!output
        .windows(b"REMOTE_STILL_SELECTED".len())
        .any(|part| part == b"REMOTE_STILL_SELECTED"));
    assert!(text(output, 80, 24).contains("REMOTE_STILL_SELECTED"));
}
