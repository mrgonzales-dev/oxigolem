use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::theme::{FAINT, ORANGE, TEXT};

pub fn draw(f: &mut Frame, area: Rect, lines: &[String]) {
    let text: Vec<Line> = lines
        .iter()
        .map(|l| {
            if let Some(cmd) = l.strip_prefix("$ ") {
                Line::from(vec![
                    Span::styled("$ ", Style::default().fg(ORANGE)),
                    Span::styled(cmd.to_string(), Style::default().fg(TEXT)),
                ])
            } else if let Some(err) = l.strip_prefix("! ") {
                Line::from(Span::styled(format!("! {err}"), Style::default().fg(ORANGE)))
            } else {
                Line::from(Span::styled(l.clone(), Style::default().fg(TEXT)))
            }
        })
        .collect();

    let output = Paragraph::new(text)
        .block(
            Block::default()
                .title(" terminal ")
                .title_style(Style::default().fg(ORANGE))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(FAINT)),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(output, area);
}
