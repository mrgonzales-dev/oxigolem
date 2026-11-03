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
pub fn height_for(input: &str, width: u16) -> u16 {
    let usable = width.saturating_sub(3) as usize; // "❯ " prefix + cursor
    if usable == 0 {
        return 2;
    }
    let lines = if input.is_empty() {
        1
    } else {
        (input.chars().count() + 1).div_ceil(usable) as u16
    };
    lines.min(MAX_LINES) + 1 // +1 for top border
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

/// `slash_prefix_len` = chars of a recognized slash-command prefix at
/// the start of `input` (e.g. 7 for `/doctor`). The input bar paints
/// those chars orange so the user can see the command will intercept
/// before they hit Enter. 0 = no highlighting.
pub fn draw(f: &mut Frame, area: Rect, input: &str, tick: u64, slash_prefix_len: usize) {
    let content_height = area.height.saturating_sub(1) as usize; // minus top border
    let usable = area.width.saturating_sub(3) as usize;
    // Blink the cursor at ~10 ticks per on/off cycle (~500ms at 50ms/tick).
    let cursor_on = tick % 20 < 12;
    let cursor = if cursor_on { "█" } else { " " };
    let cursor_style = Style::default().fg(ORANGE);

    let mut content: Vec<Line> = Vec::new();

    if input.is_empty() {
        content.push(Line::from(vec![
            Span::styled("❯ ", Style::default().fg(ORANGE).add_modifier(Modifier::BOLD)),
            Span::styled(cursor, cursor_style),
            Span::styled(" type a command…", Style::default().fg(FAINT)),
        ]));
    } else {
        let tail = wrapped_tail(input, usable.max(1), content_height.max(1));
        // Track chars already emitted so the slash prefix can be split
        // across wrapped chunks if it ever crosses a wrap boundary.
        let mut emitted = 0usize;
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
            // Split the chunk at the slash-prefix boundary if it crosses
            // one. `emitted` counts real input chars (not the prompt).
            let chunk_len = chunk.chars().count();
            let chunk_end = emitted + chunk_len;
            if slash_prefix_len > emitted && slash_prefix_len < chunk_end {
                // Boundary lands inside this chunk.
                let hot = slash_prefix_len - emitted;
                let (a, b): (String, String) = (
                    chunk.chars().take(hot).collect(),
                    chunk.chars().skip(hot).collect(),
                );
                spans.push(Span::styled(a, Style::default().fg(ORANGE)));
                spans.push(Span::styled(b, Style::default().fg(TEXT)));
            } else if slash_prefix_len > emitted {
                // Whole chunk is inside the prefix.
                spans.push(Span::styled(chunk.clone(), Style::default().fg(ORANGE)));
            } else {
                spans.push(Span::styled(chunk.clone(), Style::default().fg(TEXT)));
            }
            emitted = chunk_end;
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
