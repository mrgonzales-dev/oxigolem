use std::sync::atomic::Ordering;

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
    shell::{BgShells, BgStatus},
    theme::{BG, BLUE, FAINT, ORANGE, TEXT},
};

/// Footer bar: oxigolem chip, model, then one tab per shell —
/// orange when active (Agent view is a tab too), blue when the tab
/// is a bg shell. Running shells show no marker; finished ones get
/// their exit code or `✕` appended.
pub fn draw(
    f: &mut Frame,
    area: Rect,
    view: View,
    connected: Option<&Connection>,
    shells: &BgShells,
) {
    let model_label = match connected {
        Some(c) => format!("{}/{}", c.provider, c.model),
        None => "not connected".to_string(),
    };

    let mut spans = vec![
        Span::styled(
            " oxigolem ",
            Style::default().fg(BG).bg(ORANGE).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(model_label, Style::default().fg(ORANGE)),
        Span::styled("  ·  ", Style::default().fg(FAINT)),
    ];

    // Agent tab — plain text, orange when selected.
    let agent_selected = matches!(view, View::Agent);
    spans.push(Span::styled(
        "chat",
        if agent_selected {
            Style::default().fg(ORANGE).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(TEXT)
        },
    ));

    let reg = shells.lock().unwrap();
    for (i, sh) in reg.iter().enumerate() {
        spans.push(Span::styled("  ", Style::default()));
        let selected = matches!(view, View::Shell(n) if n == i);
        let mut label = sh.id.clone();
        match sh.status() {
            BgStatus::Running => {}
            BgStatus::Exited(c) => label.push_str(&format!("·{c}")),
            BgStatus::Killed => label.push_str("·✕"),
        }
        let transient = sh.transient.load(Ordering::Relaxed);
        let color = if transient { ORANGE } else { BLUE };
        spans.push(Span::styled(
            label,
            if selected {
                Style::default().fg(color).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(color)
            },
        ));
    }
    drop(reg);

    spans.push(Span::styled("   ctrl+p  shift+tab", Style::default().fg(FAINT)));

    let header = Paragraph::new(Line::from(spans)).block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(FAINT)),
    );
    f.render_widget(header, area);
}
