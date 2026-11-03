use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::{
    app::{Role, TranscriptEntry},
    theme::{FAINT, ORANGE, ORANGE_DIM, TEXT},
};

fn role_label(role: &Role) -> (&'static str, ratatui::style::Color) {
    match role {
        Role::User => ("user", FAINT),
        Role::Golem => ("golem", ORANGE),
        Role::Tool => ("tool", ORANGE_DIM),
    }
}

pub fn draw(f: &mut Frame, area: Rect, transcript: &[TranscriptEntry]) {
    let mut lines: Vec<Line> = Vec::new();

    for entry in transcript {
        let (label, color) = role_label(&entry.role);
        match entry.role {
            Role::Golem => {
                let md = tui_markdown::from_str(&entry.text);
                for line in md.lines {
                    let mut prefixed = Line::default();
                    prefixed.spans.push(Span::styled(
                        format!("{label:>6} "),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ));
                    prefixed.spans.extend(line.spans);
                    lines.push(prefixed);
                }
                lines.push(Line::default());
            }
            _ => {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{label:>6} "),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(entry.text.clone(), Style::default().fg(TEXT)),
                ]));
            }
        }
    }

    let block = Block::default()
        .title(" oxigolem ")
        .title_style(Style::default().fg(ORANGE))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(FAINT));

    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(block);
    f.render_widget(p, area);
}
