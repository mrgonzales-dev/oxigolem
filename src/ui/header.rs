use std::sync::atomic::Ordering;

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::{
    app::View,
    modal::Connection,
    provider::Usage,
    shell::{BgShells, BgStatus},
    theme::{BG, BLUE, FAINT, ORANGE, TEXT},
};

/// Animation state for the brand chip — computed by the caller so
/// `draw` stays a pure render function. `busy`/`connecting` toggle
/// the chip's scramble animation; `tick` drives the frame counter.
pub struct ChipState {
    pub busy: bool,
    pub connecting: bool,
    pub tick: u64,
}

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
    usage: Option<&Usage>,
    chip: ChipState,
) {
    let model_label = match connected {
        Some(c) => format!("{}/{}", c.provider, c.model),
        None => "not connected".to_string(),
    };

    // Brand chip — a soft shine sweeps left→right across the label
    // while connecting or generating (Claude-Code style). The text
    // itself never changes; only brightness moves. Idle = static.
    let chip_label = if chip.connecting {
        "connecting"
    } else {
        "oxigolem"
    };
    let mut spans: Vec<Span> = if chip.connecting || chip.busy {
        chip_spans(chip_label, chip.tick)
    } else {
        vec![Span::styled(
            " oxigolem ",
            Style::default()
                .fg(BG)
                .bg(ORANGE)
                .add_modifier(Modifier::BOLD),
        )]
    };

    // Model label — connected: an iridescent hue shimmer rolls through
    // it (per-char hue offset + slow drift — colors travel left→right
    // and cycle over time, no more stale orange). Disconnected: static
    // faint — a rainbow on a dead model lies about connection health.
    spans.push(Span::raw("  "));
    if connected.is_some() {
        spans.extend(model_spans(&model_label, chip.tick));
    } else {
        spans.push(Span::styled(model_label, Style::default().fg(FAINT)));
    }
    // Cache hit rate + generation speed from the last turn — hidden
    // until the first done chunk lands.
    if let Some(u) = usage {
        spans.push(Span::styled(
            format!(
                "  · {:.1}% cached · {:.0} t/s",
                u.cached_pct(),
                u.tokens_per_sec()
            ),
            Style::default().fg(FAINT),
        ));
    }

    spans.push(Span::styled("  ·  ", Style::default().fg(FAINT)));

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

    spans.push(Span::styled(
        "   ctrl+p  shift+tab",
        Style::default().fg(FAINT),
    ));

    let header = Paragraph::new(Line::from(spans)).block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(FAINT)),
    );
    f.render_widget(header, area);
}

/// Claude-Code-style shine: the chip text is fixed — a bright spot
/// sweeps left→right across it, dimming the chars behind it. One
/// full sweep every ~28 ticks (~1.4 s at 50 ms/tick) with a brief
/// settle at the end so it reads as a pulse, not a marquee.
/// Sped up from 40/56 — 2.4 s read as a crawl next to the 4.8 s
/// thinking shimmer.
fn chip_spans(label: &str, tick: u64) -> Vec<Span<'static>> {
    const SWEEP: u64 = 28; // ticks for the shine to cross once
    const PERIOD: u64 = 40; // sweep + rest
    let text = format!(" {label} ");
    let n = text.chars().count();
    let t = tick % PERIOD;
    // Sweep position in char-space, -2..n+2 so the blob enters from
    // the left gutter and exits right before resting.
    let pos = if t < SWEEP {
        (t as f32 / SWEEP as f32) * (n as f32 + 4.0) - 2.0
    } else {
        -10.0 // resting — no highlight
    };
    text.chars()
        .enumerate()
        .map(|(i, c)| {
            let d = (i as f32 - pos).abs();
            // Gaussian falloff — wide soft crest, no hard edge.
            let k = (-(d * d) / 6.0).exp();
            let bg = mix_rgb(ORANGE, crate::theme::THINK_HOT, k);
            Span::styled(
                c.to_string(),
                Style::default().fg(BG).bg(bg).add_modifier(Modifier::BOLD),
            )
        })
        .collect()
}

/// Model-label shimmer — per-char hue: char `col` sees hue
/// `(2°/tick) + 8°` of column offset, so a rainbow wave drifts
/// left→right across the label and every char keeps cycling. Full
/// cycle per char ≈ 9 s; neighbor chars sit 8° apart. Saturation and
/// value are held constant so it shimmers in hue only — reads as
/// iridescence, not flashing.
pub fn model_shimmer_hue(col: usize, tick: u64) -> f32 {
    // f64 keeps tick*2° exact to flyweight precision; the label sits
    // on a dark bg at fixed s/v, so wraparound is the only seam.
    let h = (tick as f64) * 2.0 + (col as f64) * 8.0;
    (h % 360.0) as f32
}

/// HSV → `ratatui` RGB. `h` in degrees (any value; wrapped mod 360),
/// `s`/`v` clamped 0..1. Standard formula, exact primaries at s=1/v=1.
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> ratatui::style::Color {
    use ratatui::style::Color;
    let s = s.clamp(0.0, 1.0);
    let v = v.clamp(0.0, 1.0);
    let h = ((h % 360.0 + 360.0) % 360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h as u8 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let f = |k: f32| ((k + m) * 255.0 + 0.5) as u8;
    Color::Rgb(f(r), f(g), f(b))
}

/// The shimmer itself: one span per char, hue from
/// [`model_shimmer_hue`], s/v pinned so only hue moves.
fn model_spans(label: &str, tick: u64) -> Vec<Span<'static>> {
    const SAT: f32 = 0.62;
    const VAL: f32 = 0.82;
    label
        .chars()
        .enumerate()
        .map(|(i, c)| {
            let color = hsv_to_rgb(model_shimmer_hue(i, tick), SAT, VAL);
            Span::styled(c.to_string(), Style::default().fg(color))
        })
        .collect()
}

/// Linear blend between two `Color::Rgb` values. `k=0` keeps `a`,
/// `k=1` gives `b`.
fn mix_rgb(a: ratatui::style::Color, b: ratatui::style::Color, k: f32) -> ratatui::style::Color {
    use ratatui::style::Color;
    let (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg_, bb)) = (a, b) else {
        return a;
    };
    let m = |x: f32, y: f32| (x + (y - x) * k.clamp(0.0, 1.0)) as u8;
    Color::Rgb(
        m(ar as f32, br as f32),
        m(ag as f32, bg_ as f32),
        m(ab as f32, bb as f32),
    )
}
