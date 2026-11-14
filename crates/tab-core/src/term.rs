//! What a command's output looks like on a terminal, as plain text.
//!
//! Programs write colours, `\r` progress bars, cursor moves and line
//! clears for a terminal. Stripping escape codes by hand leaves every
//! redraw of a progress bar in the text; feeding the bytes through a real
//! terminal emulator's screen (`alacritty_terminal`, Apache-2.0) leaves what
//! a person would have seen. This is also the screen state the designed PTY
//! view needs (see docs/ARCHITECTURE.md).

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;

/// Wide enough that most lines don't wrap; wrapped lines are joined again.
const COLUMNS: usize = 200;
const SCREEN: usize = 50;
/// Lines kept above the screen. Output longer than this keeps its end.
const MAX_HISTORY: usize = 20_000;

struct Size {
    columns: usize,
    lines: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

/// The text `bytes` leave on a terminal, from the top of its scrollback,
/// with trailing spaces and blank lines dropped.
pub fn plain(bytes: &[u8]) -> String {
    if !bytes.iter().any(|b| matches!(b, 0x1b | b'\r' | 0x08)) {
        // Nothing a terminal would redraw: the text is the text.
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let lines = bytes.iter().filter(|b| **b == b'\n').count();
    let config = Config {
        scrolling_history: (lines + 1).min(MAX_HISTORY),
        ..Config::default()
    };
    let size = Size {
        columns: COLUMNS,
        lines: SCREEN,
    };
    let mut term = Term::new(config, &size, VoidListener);
    let mut parser: Processor = Processor::new();
    // A pipe isn't a tty: nothing turned `\n` into `\r\n` on the way.
    let mut prev = 0u8;
    let mut chunk = Vec::with_capacity(bytes.len() + lines);
    for &b in bytes {
        if b == b'\n' && prev != b'\r' {
            chunk.push(b'\r');
        }
        chunk.push(b);
        prev = b;
    }
    parser.advance(&mut term, &chunk);
    let grid = term.grid();
    let text = term.bounds_to_string(
        Point::new(grid.topmost_line(), Column(0)),
        Point::new(Line(grid.screen_lines() as i32 - 1), grid.last_column()),
    );
    let mut out: Vec<&str> = text.lines().map(str::trim_end).collect();
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    while out.first().is_some_and(|l| l.is_empty()) {
        out.remove(0);
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_go_and_text_stays() {
        assert_eq!(plain(b"\x1b[32mDone\x1b[0m in 3s\n"), "Done in 3s");
    }

    #[test]
    fn a_progress_bar_leaves_its_last_frame() {
        let out = plain(b"Installing\n[##   ] 40%\r[#### ] 80%\r[#####] 100%\nok\n");
        assert_eq!(out, "Installing\n[#####] 100%\nok");
    }

    #[test]
    fn cleared_lines_and_backspaces_are_applied() {
        assert_eq!(plain(b"spinner |\x1b[2K\rdone\n"), "done");
        assert_eq!(plain(b"abc\x08\x08XY\n"), "aXY");
    }

    #[test]
    fn long_lines_are_joined_again() {
        let long = "x".repeat(COLUMNS + 20);
        assert_eq!(plain(format!("\x1b[1m{long}\x1b[0m\n").as_bytes()), long);
    }

    #[test]
    fn plain_text_is_left_alone() {
        assert_eq!(plain(b"a\n\n  b\n"), "a\n\n  b\n");
    }

    #[test]
    fn output_longer_than_the_screen_keeps_its_start() {
        let mut s = String::new();
        for i in 0..200 {
            s.push_str(&format!("\x1b[0mline {i}\n"));
        }
        let out = plain(s.as_bytes());
        assert!(out.starts_with("line 0\nline 1\n"), "{}", &out[..40]);
        assert!(out.ends_with("line 199"));
    }
}
