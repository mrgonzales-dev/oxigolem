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
    scroll_back: Option<u16>,
    gen_done_tick: Option<u64>,
) {
    let mut lines: Vec<Line> = Vec::new();
    // Left gutter so markers sit one cell in from the edge.
    let pad = "  ";
    // Right-edge margin for thinking/reply bodies — left side stays tight
    // against the marker so there's no dead column between them.
    let rmargin = "    ";

    // Width available for body text once the marker and right margin are
    // subtracted — pre-wrap thinking/golem so wrapped rows keep the right
    // margin instead of hugging the edge.
    let body_w = (area.width as usize).saturating_sub(pad.len() + 2 + rmargin.len());

    // Index of the trailing Thinking entry (if any) — only it animates;
    // older thinking blocks stay FAINT so a new chat's shimmer doesn't
    // re-light earlier reasoning.
    let last_thinking = transcript
        .iter()
        .rposition(|e| matches!(e.role, Role::Thinking));

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
                // "✻ reasoning…" — pre-wrap to body_w so every visual row
                // keeps the left margin; right margin comes free since
                // pre-wrapped rows never reach the edge.
                // Only the most recent thinking block animates — earlier
                // ones are always settled FAINT.
                let fresh = Some(ei) == last_thinking;
                let live = fresh && thinking_live && ei + 1 == transcript.len();
                // Fade shimmer → FAINT over ~20 ticks after generation ends.
                let fade = if !fresh {
                    0.0
                } else if live {
                    1.0
                } else {
                    match gen_done_tick {
                        Some(t0) => {
                            let e = tick.saturating_sub(t0) as f32;
                            (1.0 - e / 20.0).max(0.0)
                        }
                        None => 0.0,
                    }
                };
                let mut col = 0usize;
                let mut first = true;
                for raw in entry.text.lines() {
                    for l in wrap_chars(raw, body_w.max(1)) {
                        let marker = if first { "✻ " } else { "  " };
                        first = false;
                        let mut spans = vec![Span::styled(
                            pad.to_string() + marker,
                            Style::default().fg(FAINT),
                        )];
                        for c in l.chars() {
                            spans.push(Span::styled(
                                c.to_string(),
                                Style::default().fg(shade(fade, col, tick)),
                            ));
                            col += 1;
                        }
                        lines.push(Line::from(spans));
                    }
                }
                lines.push(Line::default());
            }
            Role::Golem => {
                // "● markdown…" — pre-wrap each rendered line's spans at
                // body_w so both margins survive; paragraph wrap never sees
                // a full-width row to mangle.
                let md = tui_markdown::from_str(&entry.text);
                let mut first = true;
                for line in md.lines.into_iter() {
                    for sub in wrap_spans(line.spans, body_w.max(1)) {
                        let marker = if first { "● " } else { "  " };
                        first = false;
                        let mut prefixed = Line::default();
                        prefixed.spans.push(Span::styled(
                            pad.to_string() + marker,
                            Style::default()
                                .fg(ORANGE)
                                .add_modifier(Modifier::BOLD),
                        ));
                        prefixed.spans.extend(sub);
                        lines.push(prefixed);
                    }
                }
                lines.push(Line::default());
            }
            Role::Tool => {
                // Cursor-cli style: a header `● bash · $ cmd`, an
                // indented body of stdout + `! ` stderr lines, and an
                // `✓ exit 0 · duration` footer. Output past
                // COLLAPSE_LINES collapses into a `… (N lines hidden)`
                // row until the user toggles with Ctrl+E.
                const COLLAPSE_LINES: usize = 8;

                // In-flight placeholder (ToolCall arrived, ToolResult
                // hasn't yet) renders the header alone. Chat errors and
                // other non-tool Tool entries land here too — they have
                // `text` prefixed with a marker so the header reads as
                // an error line, not a fake `bash` call.
                if entry.outcome.is_none() && entry.text.starts_with("chat error:") {
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
                    continue;
                }
                // Header — `● bash · $ cmd`. Falls back to the raw text
                // for non-bash tools (they stash args JSON in `text`).
                let (tool_name, cmd) = match entry.outcome.as_ref() {
                    Some(o) => ("bash".to_string(), o.command.clone()),
                    None => ("bash".to_string(), entry.text.clone()),
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{pad}● "),
                        Style::default()
                            .fg(ORANGE)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        tool_name,
                        Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(" · $ {cmd}"),
                        Style::default().fg(FAINT),
                    ),
                ]));

                // Body — only when we have an outcome; otherwise the call
                // is still in flight and the header alone shows intent.
                if let Some(out) = &entry.outcome {
                    let mut body: Vec<String> = Vec::new();
                    if let Some(err) = &out.result.error {
                        body.push(err.clone());
                    } else if out.result.timed_out {
                        body.push(format!(
                            "! timed out after {}s",
                            out.result.duration.as_secs()
                        ));
                    } else {
                        body.extend(out.result.stdout.lines().map(|l| l.to_string()));
                        body.extend(
                            out.result
                                .stderr
                                .lines()
                                .map(|l| format!("! {l}")),
                        );
                    }
                    let total = body.len();
                    let show = if entry.expanded || total <= COLLAPSE_LINES {
                        &body[..]
                    } else {
                        &body[..COLLAPSE_LINES]
                    };
                    for l in show {
                        lines.push(Line::from(vec![
                            Span::styled(format!("{pad}  "), Style::default()),
                            Span::styled(l.clone(), Style::default().fg(FAINT)),
                        ]));
                    }
                    if !entry.expanded && total > COLLAPSE_LINES {
                        let hidden = total - COLLAPSE_LINES;
                        lines.push(Line::from(vec![
                            Span::styled(format!("{pad}  "), Style::default()),
                            Span::styled(
                                format!("… ({hidden} lines hidden — Ctrl+E to expand)"),
                                Style::default().fg(ORANGE_DIM),
                            ),
                        ]));
                    }

                    // Footer — `✓ exit 0 · 0.04s` on success, `✗ exit N` on
                    // nonzero, `✗ timed out` when killed, `✗ <error>` on
                    // spawn/missing-command failures.
                    let (mark, summary, color) = if out.result.timed_out {
                        (
                            "✗",
                            format!("timed out after {}s", out.result.duration.as_secs()),
                            ORANGE_DIM,
                        )
                    } else if let Some(e) = &out.result.error {
                        ("✗", e.clone(), ORANGE_DIM)
                    } else {
                        match out.result.code {
                            Some(0) => ("✓", "exit 0".to_string(), ORANGE_DIM),
                            Some(c) => ("✗", format!("exit {c}"), ORANGE_DIM),
                            None => ("✗", "no exit code".to_string(), ORANGE_DIM),
                        }
                    };
                    let secs = out.result.duration.as_secs_f32();
                    lines.push(Line::from(vec![
                        Span::styled(format!("{pad}  "), Style::default()),
                        Span::styled(mark.to_string(), Style::default().fg(color)),
                        Span::styled(
                            format!(" {summary} · {secs:.2}s"),
                            Style::default().fg(FAINT),
                        ),
                    ]));
                }
                lines.push(Line::default());
            }
        }
    }

    // Scroll is in wrapped visual rows — estimate each line's row cost at
    // the render width. `scroll_back` pins a manual offset from the bottom;
    // None follows the newest entry.
    let w = area.width.max(1) as usize;
    let total: usize = lines
        .iter()
        .map(|l| (l.width().max(1)).div_ceil(w))
        .sum();
    let bottom = total.saturating_sub(area.height as usize);
    let scroll = match scroll_back {
        Some(back) => bottom.saturating_sub(back as usize) as u16,
        None => bottom as u16,
    };
    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    f.render_widget(p, area);
}

/// Wrap a plain-text line at `width` chars (hard cut — markdown sources
/// already carry their own line breaks, word wrap is cosmetic here).
fn wrap_chars(s: &str, width: usize) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    chars
        .chunks(width)
        .map(|c| c.iter().collect())
        .collect()
}

/// Wrap styled spans at `width` display cells, splitting spans at cut
/// points while preserving each span's style.
fn wrap_spans(spans: Vec<Span<'_>>, width: usize) -> Vec<Vec<Span<'_>>> {
    let mut out: Vec<Vec<Span>> = vec![Vec::new()];
    let mut col = 0usize;
    for span in spans {
        let mut text = span.content.to_string();
        let style = span.style;
        while !text.is_empty() {
            let room = width.saturating_sub(col).max(1);
            let take: usize = text.chars().take(room).count();
            let head: String = text.chars().take(take).collect();
            text = text.chars().skip(take).collect();
            out.last_mut().unwrap().push(Span::styled(head, style));
            col += take;
            if col >= width && !text.is_empty() {
                out.push(Vec::new());
                col = 0;
            }
        }
    }
    out
}

/// Color for one character of thinking text. `fade` is 1.0 while the
/// shimmer is fully on and 0.0 once it has eased back to FAINT; the sine
/// wave keeps its position while its amplitude is scaled by `fade` and
/// the whole color is lerped toward FAINT.
fn shade(fade: f32, col: usize, tick: u64) -> Color {
    if fade <= 0.0 {
        return FAINT;
    }
    // Base of the wave is a light, near-white orange so the shimmer reads
    // as a soft glow rather than a deep orange wash.
    let (r1, g1, b1) = (255.0_f32, 170.0_f32, 120.0_f32);
    let Color::Rgb(hr, hg, hb) = THINK_HOT else { return FAINT };
    let Color::Rgb(fr, fg, fb) = FAINT else { return FAINT };
    // Period ~24 columns; phase sweeps fast left→right with the frame count.
    let phase = (col as f32 - tick as f32 * 3.2) * (std::f32::consts::TAU / 24.0);
    let t = (phase.sin() + 1.0) / 2.0; // 0..1 along the wave
    let mix = |a: f32, b: f32, k: f32| a + (b - a) * k;
    // Wave color at this column, then blend the whole thing toward FAINT
    // by `fade` so the block eases out instead of snapping.
    let r = mix(mix(r1, hr as f32, t), fr as f32, 1.0 - fade);
    let g = mix(mix(g1, hg as f32, t), fg as f32, 1.0 - fade);
    let b = mix(mix(b1, hb as f32, t), fb as f32, 1.0 - fade);
    Color::Rgb(r as u8, g as u8, b as u8)
}
