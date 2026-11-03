use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::theme::{BG, ORANGE, TEXT};

pub fn draw(f: &mut Frame, area: Rect, msg: &str) {
    let width = (msg.len() as u16 + 4).min(area.width);
    let h = 3u16;
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width),
        y: area.y + area.height.saturating_sub(h),
        width,
        height: h,
    };

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
