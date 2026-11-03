use ratatui::{
    layout::{Alignment, Constraint, Flex, Layout, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::theme::{BG, ORANGE, TEXT};

pub fn draw(f: &mut Frame, area: Rect, msg: &str) {
    let width = (msg.len() as u16 + 4).min(area.width);
    let [row] = Layout::vertical([Constraint::Length(3)])
        .flex(Flex::Center)
        .areas(area);
    let [rect] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(row);

    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ORANGE))
        .style(Style::default().bg(BG));
    let p = Paragraph::new(Line::from(msg))
        .style(Style::default().fg(TEXT).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center)
        .block(block);
    f.render_widget(p, rect);
}
