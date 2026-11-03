use ratatui::{
    layout::{Constraint, Flex, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use crate::{
    modal::{Kind, Modal},
    theme::{BG, FAINT, ORANGE, TEXT},
};

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let [v] = Layout::vertical([Constraint::Length(h.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    let [r] = Layout::horizontal([Constraint::Length(w.min(area.width))])
        .flex(Flex::Center)
        .areas(v);
    r
}

pub fn draw(f: &mut Frame, area: Rect, modal: &Modal, items: &[String]) {
    let rows = items.len().clamp(1, 10) as u16;
    let rect = centered(area, 60, rows + 5);
    f.render_widget(Clear, rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ORANGE))
        .style(Style::default().bg(BG))
        .title(Span::styled(
            format!(" {} ", modal.title()),
            Style::default().fg(ORANGE).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    if let Kind::Message(msg) = &modal.kind {
        let p = Paragraph::new(vec![
            Line::from(Span::styled(msg.clone(), Style::default().fg(TEXT))),
            Line::from(""),
            Line::from(Span::styled("esc close", Style::default().fg(FAINT))),
        ])
        .wrap(Wrap { trim: false });
        f.render_widget(p, inner);
        return;
    }

    let mut lines = vec![
        Line::from(vec![
            Span::styled("› ", Style::default().fg(ORANGE)),
            Span::styled(modal.filter.clone(), Style::default().fg(TEXT)),
            Span::styled("█", Style::default().fg(ORANGE)),
        ]),
        Line::from(""),
    ];

    if items.is_empty() {
        lines.push(Line::from(Span::styled("no matches", Style::default().fg(FAINT))));
    }
    // Keep the selected row inside the 10-row window.
    let start = modal.sel.saturating_sub(9);
    for (i, item) in items.iter().enumerate().skip(start).take(10) {
        let style = if i == modal.sel {
            Style::default().fg(BG).bg(ORANGE)
        } else {
            Style::default().fg(TEXT)
        };
        lines.push(Line::from(Span::styled(format!(" {item} "), style)));
    }
    lines.push(Line::from(Span::styled(
        "↑↓ move  enter select  esc close",
        Style::default().fg(FAINT),
    )));
    f.render_widget(Paragraph::new(lines), inner);
}
