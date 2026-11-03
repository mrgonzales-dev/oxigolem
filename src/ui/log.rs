use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};

use crate::{
    app::{Role, TranscriptEntry},
    shell::BgShells,
    theme::{FAINT, LIVE_BASE, LIVE_HOT, ORANGE, ORANGE_DIM, TEXT, THINK_BASE, THINK_HOT, USER_BG},
};

/// Claude Code-style transcript: no box, no borders. The assistant speaks
/// under an orange `●`, the user under a faint `>`, tool output dimmed.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    f: &mut Frame,
    area: Rect,
    transcript: &[TranscriptEntry],
    thinking_live: bool,
    tick: u64,
    scroll_back: Option<u16>,
    gen_done_tick: Option<u64>,
    stream_tick: u64,
    shells: &BgShells,
    report_bottom: &std::cell::Cell<u16>,
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

    // Stalled-stream detection: while the engine is busy but no chunk
    // has arrived for ~0.6s, the model is assembling something (usually
    // a tool_call — Ollama delivers those in one piece at the end). In
    // that gap we suppress the thinking decode animation and show a
    // green "calling bash…" row instead.
    const STALL_TICKS: u64 = 20; // ~1.0s at 50ms/tick — generous so a
    // slow model's inter-chunk gap mid-thinking can't flicker it on
    let stalled = thinking_live && tick.saturating_sub(stream_tick) > STALL_TICKS;
    let last = transcript.last();
    let calling_bash = stalled
        && last.is_some_and(|e| {
            matches!(e.role, Role::Thinking)
                || (matches!(e.role, Role::Tool) && e.outcome.is_some())
        });

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
                let live = fresh && thinking_live && ei + 1 == transcript.len() && !calling_bash;
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
                // Total char count for scramble-edge math — the last
                // ~24 chars of a live block flicker through random
                // glyphs before settling on the real char, like the
                // stream is decoding itself.
                let total_chars: usize = entry.text.chars().count();
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
                            let shown = if live {
                                decode_char(c, col, total_chars, tick)
                            } else {
                                c
                            };
                            spans.push(Span::styled(
                                shown.to_string(),
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
                            Style::default().fg(ORANGE).add_modifier(Modifier::BOLD),
                        ));
                        prefixed.spans.extend(sub);
                        lines.push(prefixed);
                    }
                }
                lines.push(Line::default());
            }
            Role::Report => {
                // Verbatim multi-line block — used by `/doctor` and
                // other self-reports. Rendered dim with the standard
                // indent; no tool chrome since there's no command.
                for l in entry.text.lines() {
                    lines.push(Line::from(vec![
                        Span::styled(format!("{pad}  "), Style::default()),
                        Span::styled(l.to_string(), Style::default().fg(FAINT)),
                    ]));
                }
                lines.push(Line::default());
            }
            Role::Tool => {
                // Cursor-cli style: a header `● bash · $ cmd`, an
                // indented body of stdout + `! ` stderr lines, and an
                // `✓ exit 0 · duration` footer. Output past
                // COLLAPSE_LINES collapses into a `… (N lines hidden)`
                // row until the user toggles with Ctrl+E.
                const COLLAPSE_LINES: usize = 4;
                // One wrapped line can still fill the screen (e.g. a
                // 10KB classpath arg) — cap visible chars per line too.
                const LINE_CAP: usize = 240;
                let line_cap = LINE_CAP;

                // In-flight placeholder (ToolCall arrived, ToolResult
                // hasn't yet) renders the header alone. Chat errors and
                // other non-tool Tool entries land here too — they have
                // `text` prefixed with a marker so the header reads as
                // an error line, not a fake `bash` call.
                if entry.outcome.is_none()
                    && (entry.text.starts_with("chat error:")
                        || entry.text.starts_with("interrupted"))
                {
                    for l in entry.text.lines() {
                        lines.push(Line::from(vec![
                            Span::styled(format!("{pad}  ⎿ "), Style::default().fg(ORANGE_DIM)),
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
                let in_flight = entry.outcome.is_none();

                // Typewriter reveal — Ollama delivers the whole tool_call
                // in one chunk, so we synthesize the stream: ~10 chars per
                // tick (~200 chars/sec at 50 ms/tick) makes the command
                // appear as if the model is typing it instead of snapping.
                // Done entries skip the reveal (call cleared the stamp).
                let shown_cmd: String = match (entry.call_tick, in_flight) {
                    (Some(t0), true) => {
                        let elapsed = tick.saturating_sub(t0) as usize;
                        let n = (elapsed * 10).min(cmd.chars().count());
                        cmd.chars().take(n).collect()
                    }
                    _ => cmd.clone(),
                };
                let revealing = entry.call_tick.is_some()
                    && in_flight
                    && shown_cmd.chars().count() < cmd.chars().count();
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{pad}● "),
                        Style::default().fg(ORANGE).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        tool_name,
                        Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!(" · $ {shown_cmd}"), Style::default().fg(FAINT)),
                    // Block cursor while the command is still revealing.
                    if revealing {
                        Span::styled("▌".to_string(), Style::default().fg(ORANGE))
                    } else {
                        Span::raw("")
                    },
                ]));

                // Live body — while the call is in flight, tail the
                // matching bg shell's ring buffer and shimmer the
                // newest lines green so the user watches output arrive
                // like a real stream. The window is sized to the view:
                // enough rows to fill the transcript while it runs.
                if in_flight {
                    let live_lines = (area.height as usize).saturating_sub(4).clamp(8, 64);
                    let live: Vec<String> = {
                        let reg = shells.lock().unwrap();
                        reg.iter()
                            .find(|s| {
                                s.transient.load(std::sync::atomic::Ordering::Relaxed)
                                    && s.command == cmd
                                    && matches!(s.status(), crate::shell::BgStatus::Running)
                            })
                            .map(|s| {
                                // Tail directly off the ring buffers —
                                // cloning the full 10k-line snapshot
                                // every frame would hitch the UI on
                                // chatty shells.
                                let mut v: Vec<String> = s
                                    .lines
                                    .lock()
                                    .unwrap()
                                    .iter()
                                    .rev()
                                    .take(live_lines)
                                    .cloned()
                                    .collect();
                                v.reverse();
                                let errs: Vec<String> = s
                                    .err_lines
                                    .lock()
                                    .unwrap()
                                    .iter()
                                    .rev()
                                    .take(live_lines)
                                    .map(|l| format!("! {l}"))
                                    .collect();
                                v.extend(errs.into_iter().rev());
                                v
                            })
                            .unwrap_or_default()
                    };
                    let live: Vec<String> =
                        live.iter().rev().take(live_lines).rev().cloned().collect();
                    let tail: Vec<&String> = live.iter().rev().take(live_lines).collect();
                    let tail: Vec<&&String> = tail.iter().rev().collect();
                    let mut col = 0usize;
                    for l in tail {
                        let mut spans = vec![Span::styled(format!("{pad}  "), Style::default())];
                        for c in l.chars().take(line_cap) {
                            spans.push(Span::styled(
                                c.to_string(),
                                Style::default().fg(shade_live(col, tick)),
                            ));
                            col += 1;
                        }
                        lines.push(Line::from(spans));
                    }
                    if live.is_empty() {
                        lines.push(Line::from(vec![
                            Span::styled(format!("{pad}  "), Style::default()),
                            Span::styled("…".to_string(), Style::default().fg(shade_live(0, tick))),
                        ]));
                    }
                }

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
                        body.extend(out.result.stderr.lines().map(|l| format!("! {l}")));
                    }
                    let total = body.len();

                    // Post-exit fade — a command that finishes inside one
                    // frame never got an in-flight render, so for ~40
                    // ticks after the outcome lands the output tail
                    // replays with the live green shimmer easing back to
                    // FAINT. Reads as "just streamed" instead of snapping.
                    let fade = entry
                        .done_tick
                        .map(|t0| {
                            let e = tick.saturating_sub(t0) as f32;
                            (1.0 - e / 40.0).max(0.0)
                        })
                        .unwrap_or(0.0);
                    let fading = fade > 0.0 && !entry.expanded;

                    let (show, shown): (&[String], usize) = if fading {
                        // Reveal the tail — the lines that "just arrived".
                        let reveal = (area.height as usize / 2).clamp(6, 24);
                        let n = total.min(reveal);
                        (&body[total - n..], n)
                    } else if entry.expanded || total <= COLLAPSE_LINES {
                        (&body[..], total)
                    } else {
                        (&body[..COLLAPSE_LINES], COLLAPSE_LINES)
                    };
                    let mut col = 0usize;
                    for l in show {
                        let clipped = if l.chars().count() > LINE_CAP {
                            format!("{} …", l.chars().take(LINE_CAP).collect::<String>())
                        } else {
                            l.clone()
                        };
                        if fading {
                            let mut spans =
                                vec![Span::styled(format!("{pad}  "), Style::default())];
                            for c in clipped.chars() {
                                spans.push(Span::styled(
                                    c.to_string(),
                                    Style::default()
                                        .fg(shade_pair(fade, col, tick, LIVE_BASE, LIVE_HOT)),
                                ));
                                col += 1;
                            }
                            lines.push(Line::from(spans));
                        } else {
                            lines.push(Line::from(vec![
                                Span::styled(format!("{pad}  "), Style::default()),
                                Span::styled(clipped, Style::default().fg(TEXT)),
                            ]));
                        }
                    }
                    if let Some(path) = &out.spilled {
                        lines.push(Line::from(vec![
                            Span::styled(format!("{pad}  "), Style::default()),
                            Span::styled(
                                format!("⎿ full output: {path}"),
                                Style::default().fg(ORANGE_DIM),
                            ),
                        ]));
                    }
                    if !entry.expanded && total > shown {
                        let hidden = total - shown;
                        lines.push(Line::from(vec![
                            Span::styled(format!("{pad}  "), Style::default()),
                            Span::styled(
                                format!("… ({hidden} lines hidden — Ctrl+E to expand)"),
                                Style::default().fg(ORANGE_DIM),
                            ),
                        ]));
                    }

                    // Footer — `✓ exit 0 · 0.04s` on success, `✗ exit N` on
                    // nonzero, `⏳ still running` on demote/block-budget,
                    // `✗ timed out` when killed, `✗ <error>` on
                    // spawn/missing-command failures.
                    let (mark, summary, color) = if out.demoted {
                        (
                            "⏳",
                            "still running as background shell".to_string(),
                            ORANGE_DIM,
                        )
                    } else if out.result.timed_out {
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

    // "Calling bash" — while the stream is stalled the model is most
    // likely writing a tool_call (Ollama emits it in one chunk at the
    // end). Show a green bash row in that gap so the wait reads as
    // "tool incoming" instead of a dead transcript. When the real
    // ToolCall lands, its `● bash · $ cmd` entry takes over from here.
    if calling_bash {
        // Animated dots — cycles … → blank so it reads as waiting.
        let dots = ". ".repeat(((tick / 6) % 4) as usize);
        let mut spans = vec![
            Span::styled(
                format!("{pad}● "),
                Style::default().fg(LIVE_HOT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "bash",
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" · calling", Style::default().fg(FAINT)),
        ];
        for (i, c) in dots.chars().enumerate() {
            spans.push(Span::styled(
                c.to_string(),
                Style::default().fg(shade_live(i, tick)),
            ));
        }
        lines.push(Line::from(spans));
        lines.push(Line::default());
    }

    // Scroll is in wrapped visual rows — estimate each line's row cost at
    // the render width. `scroll_back` pins a manual offset from the bottom;
    // None follows the newest entry.
    let w = area.width.max(1) as usize;
    let mut total: usize = lines.iter().map(|l| (l.width().max(1)).div_ceil(w)).sum();

    // Paragraph::scroll takes a u16 — a transcript past 65,535 rows
    // truncates the offset and the view locks near the top with the
    // tail unreachable. Cap the render to the newest ~60k rows by
    // dropping leading lines; nobody scrolls 60k rows back anyway.
    const MAX_ROWS: usize = 60_000;
    if total > MAX_ROWS {
        let mut dropped = Vec::with_capacity(lines.len());
        let mut keep_from = 0usize;
        let mut rows_seen = 0usize;
        for (i, l) in lines.iter().enumerate() {
            rows_seen += (l.width().max(1)).div_ceil(w);
            if total - rows_seen <= MAX_ROWS {
                keep_from = i;
                break;
            }
        }
        dropped.extend(lines.drain(keep_from..));
        lines = dropped;
        total = lines.iter().map(|l| (l.width().max(1)).div_ceil(w)).sum();
    }

    let bottom = total.saturating_sub(area.height as usize);
    report_bottom.set(bottom.min(u16::MAX as usize) as u16);
    let scroll = match scroll_back {
        // Clamp the pin to the real range so a stale or overflowed
        // value can never lock the view at the top.
        Some(back) => bottom.saturating_sub((back as usize).min(bottom)) as u16,
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
    chars.chunks(width).map(|c| c.iter().collect()).collect()
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

/// Decode effect for the trailing edge of a live thinking block: the
/// last ~24 chars of the stream cycle through random glyphs before
/// locking to the real char. Deterministic on (col, tick) so each
/// char flickers at its own pace; chars further from the edge settle
/// sooner. Returns the char to draw this frame.
fn decode_char(real: char, col: usize, total: usize, tick: u64) -> char {
    // Distance from the end of the text. Once a char is more than
    // ~24 positions behind the leading edge it's fully settled.
    let dist_from_edge = total.saturating_sub(col + 1);
    if dist_from_edge > 24 {
        return real;
    }
    // Per-char hash picks a settle delay — earlier chars (larger
    // dist) have mostly settled already; the very edge flickers.
    let mut h = (col as u32).wrapping_mul(0x9e3779b9);
    h ^= h << 13;
    h ^= h >> 17;
    h ^= h << 5;
    // Each char settles after `settle_at` ticks since it appeared;
    // approximate "age" as distance-from-edge * chars-per-tick.
    // The stream adds roughly 1 char/tick, so age ≈ dist_from_edge.
    let settle_at = 4 + (h % 16) as usize; // 4..20 ticks of flicker
    if dist_from_edge > settle_at {
        return real;
    }
    // Still scrambling — pick a glyph from a matrix-flavoured set,
    // rehashing with tick so the char changes every frame.
    const GLYPHS: &[char] = &[
        'ｱ', 'ｲ', 'ｳ', 'ｴ', 'ｵ', 'ｶ', 'ｷ', 'ｸ', 'ｹ', 'ｺ', 'ｻ', 'ｼ', 'ｽ', 'ｾ', 'ｿ', 'ﾀ', 'ﾁ', 'ﾂ',
        'ﾃ', 'ﾄ', 'ﾅ', 'ﾆ', 'ﾇ', 'ﾈ', 'ﾉ', 'ﾊ', 'ﾋ', 'ﾌ', 'ﾍ', 'ﾎ', '0', '1', '2', '3', '4', '5',
        '6', '7', '8', '9', '+', '*', ':', '¦', '⟊',
    ];
    let mut h2 = h.wrapping_add(tick as u32);
    h2 ^= h2 << 13;
    h2 ^= h2 >> 17;
    h2 ^= h2 << 5;
    GLYPHS[(h2 as usize) % GLYPHS.len()]
}

/// Thinking palette — orange shimmer on the reasoning stream.
fn shade(fade: f32, col: usize, tick: u64) -> Color {
    shade_pair(fade, col, tick, THINK_BASE, THINK_HOT)
}

/// Live-tool palette — matrix green for the in-flight bash tail.
fn shade_live(col: usize, tick: u64) -> Color {
    shade_pair(1.0, col, tick, LIVE_BASE, LIVE_HOT)
}

/// Color for one character of shimmered text. `fade` is 1.0 while the
/// shimmer is fully on and 0.0 once it has eased back to FAINT.
/// Lava-lamp feel: a wide sine window (period ~96 cols) drifts
/// left→right at 4 cols / tick (a full sweep in ~1.2 s at 50 ms/tick —
/// 4× from the 1 col/tick the user kept calling a glacier; powf(3)
/// keeps the crest a blob even as it moves fast).
fn shade_pair(fade: f32, col: usize, tick: u64, base: Color, hot: Color) -> Color {
    if fade <= 0.0 {
        return FAINT;
    }
    let Color::Rgb(br, bg_, bb) = base else {
        return FAINT;
    };
    let Color::Rgb(hr, hg, hb) = hot else {
        return FAINT;
    };
    let Color::Rgb(fr, fg, fb) = FAINT else {
        return FAINT;
    };
    // Drift: 4 cols/tick — full sweep of the ~96-col window in ~1.2 s
    // (4× from 1 col/tick). Fast, and powf(3) keeps it a blob not a
    // stripe at speed.
    let phase = (col as f32 - tick as f32 * 4.0) * (std::f32::consts::TAU / 96.0);
    // powf(3) on the upper half of the sine widens the bright blob and
    // flattens the trough — reads as a lava blob, not a stripe.
    let t = ((phase.sin() + 1.0) / 2.0).powf(3.0);
    let mix = |a: f32, b: f32, k: f32| a + (b - a) * k;
    let r = mix(mix(br as f32, hr as f32, t), fr as f32, 1.0 - fade);
    let g = mix(mix(bg_ as f32, hg as f32, t), fg as f32, 1.0 - fade);
    let b = mix(mix(bb as f32, hb as f32, t), fb as f32, 1.0 - fade);
    Color::Rgb(r as u8, g as u8, b as u8)
}
