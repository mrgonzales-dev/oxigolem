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
    let chip_label = if chip.connecting { "connecting" } else { "oxigolem" };
    let mut spans: Vec<Span> = if chip.connecting || chip.busy {
        chip_spans(chip_label, chip.tick)
    } else {
        vec![Span::styled(
            " oxigolem ",
            Style::default().fg(BG).bg(ORANGE).add_modifier(Modifier::BOLD),
        )]
    };

    spans.extend([
        Span::raw("  "),
        Span::styled(model_label, Style::default().fg(ORANGE)),
    ]);

    // Cache hit rate + generation speed from the last turn — hidden
    // until the first done chunk lands.
    if let Some(u) = usage {
        spans.push(Span::styled(
            format!("  · {:.1}% cached · {:.0} t/s", u.cached_pct(), u.tokens_per_sec()),
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

    spans.push(Span::styled("   ctrl+p  shift+tab", Style::default().fg(FAINT)));

    let header = Paragraph::new(Line::from(spans)).block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(FAINT)),
    );
    f.render_widget(header, area);
}

/// Claude-Code-style shine: the chip text is fixed — a bright spot
/// sweeps left→right across it, dimming the chars behind it. One
/// full sweep every ~48 ticks (~2.4 s at 50 ms/tick) with a brief
/// settle at the end so it reads as a pulse, not a marquee.
fn chip_spans(label: &str, tick: u64) -> Vec<Span<'static>> {
    const SWEEP: u64 = 40; // ticks for the shine to cross once
    const PERIOD: u64 = 56; // sweep + rest
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

/// Linear blend between two `Color::Rgb` values. `k=0` keeps `a`,
/// `k=1` gives `b`.
fn mix_rgb(a: ratatui::style::Color, b: ratatui::style::Color, k: f32) -> ratatui::style::Color {
    use ratatui::style::Color;
    let (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg_, bb)) = (a, b) else {
        return a;
    };
    let m = |x: f32, y: f32| (x + (y - x) * k.clamp(0.0, 1.0)) as u8;
    Color::Rgb(m(ar as f32, br as f32), m(ag as f32, bg_ as f32), m(ab as f32, bb as f32))
}
