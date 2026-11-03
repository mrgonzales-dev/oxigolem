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

    // Brand chip — glows/scrambles while connecting or generating so
    // the user can see work is happening at a glance. Idle = static.
    let chip_span = if chip.connecting {
        chip_frame("connecting", chip.tick)
    } else if chip.busy {
        chip_frame("oxigolem", chip.tick)
    } else {
        Span::styled(
            " oxigolem ",
            Style::default().fg(BG).bg(ORANGE).add_modifier(Modifier::BOLD),
        )
    };

    let mut spans = vec![
        chip_span,
        Span::raw("  "),
        Span::styled(model_label, Style::default().fg(ORANGE)),
    ];

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

/// Per-tick pseudo-random animation for the brand chip. Two sources of
/// randomness, both deterministic on `tick`:
///   - xorshift32 hash of the tick picks one of several style modes
///   - a second hash picks a text variant (dots, brackets, caps, etc.)
///
/// The result is a fresh look every frame — color, background, text,
/// modifiers all change. Idle uses the static chip so this only fires
/// while work is actually happening.
fn chip_frame(label: &str, tick: u64) -> Span<'static> {
    // Advance one frame every ~3 ticks (~150 ms at 50 ms/tick) — fast
    // enough to feel alive, slow enough each variant is readable.
    let frame = tick / 3;
    // xorshift32 — cheap deterministic hash, no rand crate needed.
    let mut h = (frame as u32).wrapping_add(0x9e3779b9);
    h ^= h << 13;
    h ^= h >> 17;
    h ^= h << 5;
    let mode = (h % 8) as usize;
    let h2 = h.wrapping_mul(0x85ebca6b) ^ (h >> 13);
    let variant = (h2 % 8) as usize;

    // Text variants — brackets, dots, stars, uppercase, spaced, etc.
    let text: String = match variant {
        0 => format!(" {} ", label),
        1 => format!(" [{}] ", label),
        2 => format!("  {}  ", label.to_uppercase()),
        3 => format!(" ·{}· ", label),
        4 => format!(" ✦ {} ✦ ", label),
        5 => format!("  {}  ", label.chars().map(|c| format!("{c} ")).collect::<String>().trim_end()),
        6 => format!(" ⟨{}⟩ ", label),
        _ => format!(" {}… ", label),
    };

    // Style modes — foreground/background combinations plus modifier
    // flourishes (bold, italic, dim, reversed, underlined).
    let style = match mode {
        0 => Style::default().fg(BG).bg(ORANGE).add_modifier(Modifier::BOLD),
        1 => Style::default().fg(ORANGE).add_modifier(Modifier::BOLD | Modifier::ITALIC),
        2 => Style::default().fg(BG).bg(FAINT).add_modifier(Modifier::BOLD),
        3 => Style::default().fg(ORANGE).add_modifier(Modifier::UNDERLINED | Modifier::BOLD),
        4 => Style::default().fg(FAINT).bg(BG).add_modifier(Modifier::ITALIC),
        5 => Style::default().fg(BG).bg(TEXT).add_modifier(Modifier::BOLD),
        6 => Style::default().fg(ORANGE).add_modifier(Modifier::DIM | Modifier::ITALIC),
        _ => Style::default().fg(BG).bg(ORANGE).add_modifier(Modifier::REVERSED | Modifier::BOLD),
    };

    Span::styled(text, style)
}
