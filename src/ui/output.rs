use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::{
    shell::{BgShell, BgStatus},
    theme::{BLUE, FAINT, ORANGE, TEXT},
};

/// Render one background shell's ring buffer. Stdout lines first,
/// then stderr as `! `-prefixed lines (streams aren't interleaved —
/// the log files keep each stream whole). Blue accent separates this
/// surface from the orange agent view.
pub fn draw(
    f: &mut Frame,
    area: Rect,
    sh: &BgShell,
    scroll_back: Option<u16>,
    report_bottom: &std::cell::Cell<u16>,
) {
    let mut lines: Vec<String> = sh.snapshot();
    let status_label = match sh.status() {
        BgStatus::Running => "running".to_string(),
        BgStatus::Exited(c) => format!("exited {c}"),
        BgStatus::Killed => "killed".to_string(),
    };
    lines.push(format!("── {} {} ──", sh.id, status_label));

    let text: Vec<Line> = lines
        .iter()
        .map(|l| {
            if let Some(err) = l.strip_prefix("! ") {
                Line::from(Span::styled(
                    format!("! {err}"),
                    Style::default().fg(ORANGE),
                ))
            } else {
                Line::from(Span::styled(l.clone(), Style::default().fg(TEXT)))
            }
        })
        .collect();

    // Scroll is in wrapped visual rows — same pin-from-bottom math as
    // the agent log so the views behave identically.
    let w = area.width.saturating_sub(2).max(1) as usize; // minus borders
    let total: usize = text.iter().map(|l| (l.width().max(1)).div_ceil(w)).sum();
    let inner_h = area.height.saturating_sub(2) as usize;
    let bottom = total.saturating_sub(inner_h);
    report_bottom.set(bottom.min(u16::MAX as usize) as u16);
    let scroll = match scroll_back {
        Some(back) => bottom.saturating_sub((back as usize).min(bottom)) as u16,
        None => bottom as u16,
    };

    let output = Paragraph::new(text)
        .block(
            Block::default()
                .title(format!(" {} ", sh.command))
                .title_style(Style::default().fg(BLUE))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(FAINT)),
        )
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    f.render_widget(output, area);
}
