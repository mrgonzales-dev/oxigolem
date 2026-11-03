use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::{
    app::View,
    modal::Connection,
    theme::{BG, FAINT, ORANGE},
};

pub fn draw(f: &mut Frame, area: Rect, view: View, connected: Option<&Connection>) {
    let model_label = match connected {
        Some(c) => format!("{}/{}", c.provider, c.model),
        None => "not connected".to_string(),
    };
    let view_label = match view {
        View::Agent => "oxigolem",
        View::Output => "terminal",
    };

    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            " oxigolem ",
            Style::default().fg(BG).bg(ORANGE).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(model_label, Style::default().fg(ORANGE)),
        Span::styled("  ·  ", Style::default().fg(FAINT)),
        Span::styled(view_label, Style::default().fg(ORANGE).add_modifier(Modifier::BOLD)),
        Span::styled("  ·  ", Style::default().fg(FAINT)),
        Span::styled("ctx 0/256k", Style::default().fg(FAINT)),
        Span::styled("  ·  ", Style::default().fg(FAINT)),
        Span::styled("step 0", Style::default().fg(FAINT)),
        Span::styled("          ctrl+p commands  shift+Tab switch view", Style::default().fg(FAINT)),
    ]))
    .block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(FAINT)),
    );
    f.render_widget(header, area);
}
