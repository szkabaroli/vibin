//! A custom style modifier and the backend wrapper that renders it.
//!
//! ratatui's `Modifier` is a u16 bitflags with nine bits defined; the
//! spare bits travel through `Style`, `Cell`, and the buffer diff
//! untouched. [`UNDERCURL`] claims one for wavy underlines (SGR 4:3),
//! which ratatui cannot express natively. The stock crossterm backend
//! ignores unknown bits, so [`UndercurlBackend`] wraps it and renders
//! undercurl cells itself — curl color taken from the cell's
//! `underline_color`.

use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use ratatui::style::{Color, Modifier};
use std::io::{self, Write};

/// Wavy undercurl, as a plain style modifier (bit 10 — unused by ratatui).
pub const UNDERCURL: Modifier = Modifier::from_bits_retain(0b0010_0000_0000);

/// The terminal type the app runs on.
pub type VibinTerminal = ratatui::Terminal<UndercurlBackend<io::Stdout>>;

pub struct UndercurlBackend<W: Write>(pub CrosstermBackend<W>);

impl UndercurlBackend<io::Stdout> {
    pub fn stdout() -> Self {
        UndercurlBackend(CrosstermBackend::new(io::stdout()))
    }
}

fn sgr_color(prefix: u8, c: Color) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("\x1b[{prefix}8;2;{r};{g};{b}m"),
        Color::Indexed(i) => format!("\x1b[{prefix}8;5;{i}m"),
        _ => String::new(),
    }
}

impl<W: Write> Backend for UndercurlBackend<W> {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let mut batch: Vec<(u16, u16, &Cell)> = Vec::new();
        for (x, y, cell) in content {
            let style = cell.style();
            if !style.add_modifier.contains(UNDERCURL) {
                batch.push((x, y, cell));
                continue;
            }
            // flush ordinary cells, then paint this one ourselves with the
            // full SGR state: colors, 4:3 undercurl, 58 underline color
            self.0.draw(batch.drain(..).map(|(x, y, c)| (x, y, c)))?;
            let fg = style.fg.map(|c| sgr_color(3, c)).unwrap_or_default();
            let bg = style.bg.map(|c| sgr_color(4, c)).unwrap_or_default();
            let curl = style.underline_color.map(|c| sgr_color(5, c)).unwrap_or_default();
            write!(
                self.0,
                "\x1b[{};{}H\x1b[0m{fg}{bg}\x1b[4:3m{curl}{}\x1b[0m",
                y + 1,
                x + 1,
                cell.symbol()
            )?;
        }
        self.0.draw(batch.into_iter())
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.0.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.0.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.0.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.0.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.0.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.0.clear_region(clear_type)
    }

    fn size(&self) -> io::Result<Size> {
        self.0.size()
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.0.window_size()
    }

    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.0)
    }
}
