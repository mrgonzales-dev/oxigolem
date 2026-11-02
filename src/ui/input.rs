use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::theme::{FAINT, ORANGE, TEXT};

pub fn draw(f: &mut Frame, area: Rect, input: &str) {
    let content = if input.is_empty() {
        vec![
            Span::styled("❯ ", Style::default().fg(ORANGE).add_modifier(Modifier::BOLD)),
            Span::styled("type a command…", Style::default().fg(FAINT)),
        ]
    } else {
        vec![
            Span::styled("❯ ", Style::default().fg(ORANGE).add_modifier(Modifier::BOLD)),
            Span::styled(input.to_string(), Style::default().fg(TEXT)),
            Span::styled("█", Style::default().fg(ORANGE)),
        ]
    };

    let widget = Paragraph::new(Line::from(content)).block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(FAINT)),
    );
    f.render_widget(widget, area);
}
