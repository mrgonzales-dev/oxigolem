use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};

use crate::{
    app::{Role, TranscriptEntry},
    theme::{FAINT, ORANGE, ORANGE_DIM, TEXT},
};

/// Claude Code-style transcript: no box, no borders. The assistant speaks
/// under an orange `●`, the user under a faint `>`, tool output dimmed.
pub fn draw(f: &mut Frame, area: Rect, transcript: &[TranscriptEntry]) {
    let mut lines: Vec<Line> = Vec::new();
    // Left gutter so markers sit one cell in from the edge.
    let pad = "  ";

    for entry in transcript {
        match entry.role {
            Role::User => {
                // "> text" — the user's own words, faint marker.
                for (i, l) in entry.text.lines().enumerate() {
                    let marker = if i == 0 { "> " } else { "  " };
                    lines.push(Line::from(vec![
                        Span::styled(pad.to_string() + marker, Style::default().fg(FAINT)),
                        Span::styled(l.to_string(), Style::default().fg(TEXT)),
                    ]));
                }
                lines.push(Line::default());
            }
            Role::Golem => {
                // "● markdown…" — assistant under the orange bullet.
                let md = tui_markdown::from_str(&entry.text);
                for (i, line) in md.lines.into_iter().enumerate() {
                    let mut prefixed = Line::default();
                    let marker = if i == 0 { "● " } else { "  " };
                    prefixed.spans.push(Span::styled(
                        pad.to_string() + marker,
                        Style::default()
                            .fg(ORANGE)
                            .add_modifier(Modifier::BOLD),
                    ));
                    prefixed.spans.extend(line.spans);
                    lines.push(prefixed);
                }
                lines.push(Line::default());
            }
            Role::Tool => {
                // "  ⎿ text" — tool output indented, dimmed, like Claude Code.
                for l in entry.text.lines() {
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("{pad}  ⎿ "),
                            Style::default().fg(ORANGE_DIM),
                        ),
                        Span::styled(l.to_string(), Style::default().fg(FAINT)),
                    ]));
                }
                lines.push(Line::default());
            }
        }
    }

    let p = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(p, area);
}
