use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::theme::{FAINT, ORANGE, TEXT};

/// Max content lines the input box can grow to before scrolling.
const MAX_LINES: u16 = 3;

/// Number of terminal rows the input area needs (border + wrapped lines, capped).
/// `queued` prompts each take one row above the input line.
pub fn height_for(input: &str, queued: &[String], width: u16) -> u16 {
    let usable = width.saturating_sub(3) as usize; // "❯ " prefix + cursor
    if usable == 0 {
        return 2;
    }
    let lines = if input.is_empty() {
        1
    } else {
        (input.chars().count() + 1).div_ceil(usable) as u16
    };
    queued.len() as u16 + lines.min(MAX_LINES) + 1 // +1 for top border
}

/// Wrap `text` into lines of `usable` chars and return the last `max` of them,
/// prefixing the first with "❯ ".
fn wrapped_tail(input: &str, usable: usize, max: usize) -> Vec<String> {
    let chars: Vec<char> = input.chars().collect();
    let mut lines: Vec<String> = chars
        .chunks(usable)
        .map(|c| c.iter().collect())
        .collect();
    if lines.is_empty() {
        lines.push(String::new());
    }
    let start = lines.len().saturating_sub(max);
    lines[start..].to_vec()
}

pub fn draw(f: &mut Frame, area: Rect, input: &str, queued: &[String], tick: u64) {
    let content_height = area.height.saturating_sub(1 + queued.len() as u16) as usize; // minus border + queue rows
    let usable = area.width.saturating_sub(3) as usize;
    // Blink the cursor at ~10 ticks per on/off cycle (~500ms at 50ms/tick).
    let cursor_on = tick % 20 < 12;
    let cursor = if cursor_on { "█" } else { " " };
    let cursor_style = Style::default().fg(ORANGE);

    let mut content: Vec<Line> = Vec::new();
    // Queued prompts render dimmed above the input line.
    for q in queued {
        content.push(Line::from(vec![
            Span::styled("⏳ ", Style::default().fg(ORANGE)),
            Span::styled(q.clone(), Style::default().fg(FAINT)),
        ]));
    }

    if input.is_empty() {
        content.push(Line::from(vec![
            Span::styled("❯ ", Style::default().fg(ORANGE).add_modifier(Modifier::BOLD)),
            Span::styled(cursor, cursor_style),
            Span::styled(" type a command…", Style::default().fg(FAINT)),
        ]));
    } else {
        let tail = wrapped_tail(input, usable.max(1), content_height.max(1));
        for (i, chunk) in tail.iter().enumerate() {
            let mut spans = Vec::new();
            if i == 0 && tail.len() <= content_height {
                // first visible line: show prompt only when it's actually line 0
                spans.push(Span::styled(
                    if total_lines(input, usable.max(1)) <= content_height && i == 0 {
                        "❯ "
                    } else {
                        "… "
                    },
                    Style::default().fg(ORANGE).add_modifier(Modifier::BOLD),
                ));
            }
            spans.push(Span::styled(chunk.clone(), Style::default().fg(TEXT)));
            // cursor block on very last visible chunk
            if i == tail.len() - 1 {
                spans.push(Span::styled(cursor, cursor_style));
            }
            content.push(Line::from(spans));
        }
    }

    let widget = Paragraph::new(content)
        .block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(FAINT)),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(widget, area);
}

fn total_lines(input: &str, usable: usize) -> usize {
    if input.is_empty() {
        1
    } else {
        (input.chars().count() + 1).div_ceil(usable)
    }
}
