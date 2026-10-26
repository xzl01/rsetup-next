use ratatui::{
    style::{Color, Style},
    widgets::{Block, Borders},
};

pub(in crate::tui) const SIGNAL: Color = Color::Rgb(199, 255, 74);
pub(in crate::tui) const AMBER: Color = Color::Rgb(255, 179, 65);
pub(in crate::tui) const CORAL: Color = Color::Rgb(255, 90, 73);
pub(in crate::tui) const INK: Color = Color::Rgb(16, 18, 15);
pub(in crate::tui) const BONE: Color = Color::Rgb(232, 227, 213);
pub(in crate::tui) const MUTED: Color = Color::Rgb(139, 145, 128);

pub(in crate::tui) fn instrument(title: &str) -> Block<'_> {
    Block::default()
        .title(format!(" {title} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(76, 82, 68)))
}
