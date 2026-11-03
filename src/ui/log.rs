use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};

use crate::{
    app::{Role, TranscriptEntry},
    theme::{FAINT, ORANGE, ORANGE_DIM, TEXT, THINK_HOT, USER_BG},
};

/// Claude Code-style transcript: no box, no borders. The assistant speaks
/// under an orange `●`, the user under a faint `>`, tool output dimmed.
pub fn draw(
    f: &mut Frame,
    area: Rect,
    transcript: &[TranscriptEntry],
    thinking_live: bool,
    tick: u64,
) {
    let mut lines: Vec<Line> = Vec::new();
    // Left gutter so markers sit one cell in from the edge.
    let pad = "  ";
    // Extra indent for thinking/reply bodies past the marker.
    let body = "  ";

    for (ei, entry) in transcript.iter().enumerate() {
        match entry.role {
            Role::User => {
                // "> text" — the user's own words on a gray highlight strip.
                let bg = Style::default().bg(USER_BG);
                let w = area.width as usize;
                for (i, l) in entry.text.lines().enumerate() {
                    let marker = if i == 0 { "> " } else { "  " };
                    let head = format!("{pad}{marker}");
                    let used = head.chars().count() + l.chars().count();
                    // Pad to a multiple of the view width so the highlight
                    // fills every wrapped visual line end-to-end.
                    let fill = if w == 0 { 0 } else { (w - used % w) % w };
                    lines.push(Line::from(vec![
                        Span::styled(head, bg.fg(FAINT)),
                        Span::styled(l.to_string(), bg.fg(TEXT)),
                        Span::styled(" ".repeat(fill), bg),
                    ]));
                }
                lines.push(Line::default());
            }
            Role::Thinking => {
                // "✻ reasoning…" — while the model is mid-generation the
                // last thinking block gets a traveling orange→white shimmer;
                // once done it settles to plain FAINT text.
                let live = thinking_live && ei + 1 == transcript.len();
                let mut col = 0usize;
                for (i, l) in entry.text.lines().enumerate() {
                    let marker = if i == 0 { "✻ " } else { "  " };
                    let mut spans = vec![Span::styled(
                        pad.to_string() + marker,
                        Style::default().fg(FAINT),
                    )];
                    for c in format!("{body}{l}{body}").chars() {
                        spans.push(Span::styled(
                            c.to_string(),
                            Style::default().fg(shade(live, col, tick)),
                        ));
                        col += 1;
                    }
                    lines.push(Line::from(spans));
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
                    prefixed.spans.push(Span::raw(body));
                    prefixed.spans.extend(line.spans);
                    prefixed.spans.push(Span::raw(body));
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

    // Auto-scroll to the newest entry. Scroll is in wrapped visual rows,
    // so estimate each line's row cost at the render width.
    let w = area.width.max(1) as usize;
    let total: usize = lines
        .iter()
        .map(|l| (l.width().max(1)).div_ceil(w))
        .sum();
    let scroll = total.saturating_sub(area.height as usize) as u16;
    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    f.render_widget(p, area);
}

/// Color for one character of live thinking text: a sine wave between
/// ORANGE and THINK_HOT traveling rightward as `tick` advances.
fn shade(live: bool, col: usize, tick: u64) -> Color {
    if !live {
        return FAINT;
    }
    // Base of the wave is a light, near-white orange so the shimmer reads
    // as a soft glow rather than a deep orange wash.
    let (r1, g1, b1) = (255, 170, 120);
    let Color::Rgb(r2, g2, b2) = THINK_HOT else { return THINK_HOT };
    // Period ~24 columns; phase sweeps fast left→right with the frame count.
    let phase = (col as f32 - tick as f32 * 3.2) * (std::f32::consts::TAU / 24.0);
    let t = (phase.sin() + 1.0) / 2.0; // 0..1
    let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
    Color::Rgb(lerp(r1, r2), lerp(g1, g2), lerp(b1, b2))
}
