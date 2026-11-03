//! Circular oscillator overlay — the "scope". Drawn top-right while
//! a generation streams. Three live inputs shape the trace:
//!
//!   - token rate  → angular velocity (faster gen = faster sweep)
//!   - RAM pressure → base radius (fuller memory = wider orbit)
//!   - CPU load    → radial ripple (busier cores = wobblier ring)
//!
//! Everything renders in braille (2×4 dots per cell) so a ~22×10 cell
//! box still reads as a smooth circle. No canvas dependency — a tiny
//! hand-rolled dot buffer is all this needs.

use std::f32::consts::TAU;

use ratatui::{
    layout::Rect,
    style::Style,
    widgets::{Block, Borders, Clear},
    Frame,
};

use crate::theme::{BG, FAINT, ORANGE, ORANGE_DIM};

/// Live inputs the trace consumes each frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScopeInput {
    /// Estimated tokens/sec arriving right now (EWMA from App).
    pub tps: f32,
    /// System memory pressure 0..1 (1 - MemAvailable/MemTotal).
    pub mem: f32,
    /// CPU busy fraction 0..1 sampled over the last interval.
    pub cpu: f32,
}

/// Braille dot bit per (x, y) inside a cell. Braille encodes columns
/// as bits 0-3 left / 4-7 right.
fn braille_bit(x: usize, y: usize) -> u8 {
    match (x, y) {
        (0, 0) => 0x01,
        (0, 1) => 0x02,
        (0, 2) => 0x04,
        (0, 3) => 0x40,
        (1, 0) => 0x08,
        (1, 1) => 0x10,
        (1, 2) => 0x20,
        (1, 3) => 0x80,
        _ => 0,
    }
}

/// Set a dot in the buffer — `dots` is cell-major, 2×4 dots per cell.
fn plot(dots: &mut [u8], w_cells: usize, h_cells: usize, x: f32, y: f32) {
    if x < 0.0 || y < 0.0 {
        return;
    }
    let px = x as usize;
    let py = y as usize;
    let (cx, dx) = (px / 2, px % 2);
    let (cy, dy) = (py / 4, py % 4);
    if cx < w_cells && cy < h_cells {
        dots[cy * w_cells + cx] |= braille_bit(dx, dy);
    }
}

/// Trace one frame of the oscillator into `dots`.
///
/// The parametric curve is a rotating lissajous-ish ring:
///   r(θ) = R_base * (1 + ripple * sin(harm*θ + phase2))
///   x = cx + r cos(θ + phase),  y = cy + r sin(θ + phase)
///
/// `phase` advances with token rate (plus a slow idle spin so the
/// scope never looks frozen while the model thinks).
/// `harm`/`phase2` come from CPU: busier machine = more wobble.
fn trace(
    dots: &mut [u8],
    w_cells: usize,
    h_cells: usize,
    input: ScopeInput,
    phase: f32,
) {
    let w_px = w_cells * 2;
    let h_px = h_cells * 4;
    let (cx, cy) = (w_px as f32 / 2.0, h_px as f32 / 2.0);

    // Cells are ~2× taller than wide in pixels (2×4 dots in a ~1:2
    // cell aspect) — scale x by 0.5 so the ring reads as a circle.
    let max_r = (w_px.min(h_px * 2) as f32 / 2.0 - 1.0).max(1.0);

    // Memory → base radius: 55%..95% of the drawable radius.
    let r_base = max_r * (0.55 + 0.40 * input.mem.clamp(0.0, 1.0));

    // CPU → radial ripple: 0..30% amplitude, harmonic 3..7.
    let ripple = 0.30 * input.cpu.clamp(0.0, 1.0);
    let harm = 3.0 + (input.cpu.clamp(0.0, 1.0) * 4.0).round();
    let phase2 = phase * 1.7;

    // Sample enough points that the braille line has no gaps.
    let steps = (max_r * TAU * 2.0) as usize;
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let theta = t * TAU;
        let r = r_base * (1.0 + ripple * (harm * theta + phase2).sin());
        let a = theta + phase;
        // Squash y by 0.5 to counter the 2:1 cell aspect.
        let x = cx + r * a.cos();
        let y = cy + r * a.sin() * 0.5;
        plot(dots, w_cells, h_cells, x, y);
    }
}

/// Phase advance per tick for a given token rate. Idle spins slowly
/// (~1 rev / 10 s); at 60 t/s it's ~4× that. Saturates so absurd
/// rates can't strobe the display.
pub fn phase_step(tps: f32) -> f32 {
    const IDLE: f32 = TAU / 200.0; // one rev per ~10 s at 50 ms/tick
    const PER_TPS: f32 = TAU / 1400.0;
    IDLE + PER_TPS * tps.min(120.0)
}

/// Fraction of RAM in use: 1 - MemAvailable/MemTotal. 0.5 fallback.
pub fn mem_pressure() -> f32 {
    let text = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let (mut total, mut avail) = (0u64, 0u64);
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total = rest.split_whitespace().next().and_then(|v| v.parse().ok()).unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            avail = rest.split_whitespace().next().and_then(|v| v.parse().ok()).unwrap_or(0);
        }
    }
    if total == 0 {
        return 0.5;
    }
    1.0 - (avail as f32 / total as f32).clamp(0.0, 1.0)
}

/// Instantaneous CPU busy fraction vs the previous /proc/stat read.
/// `prev` is (idle, total) jiffies; caller keeps it across calls.
/// First call returns 0.0 — there's no delta yet.
pub fn cpu_busy(prev: &mut Option<(u64, u64)>) -> f32 {
    let text = std::fs::read_to_string("/proc/stat").unwrap_or_default();
    let line = match text.lines().next() {
        Some(l) => l,
        None => return 0.0,
    };
    let fields: Vec<u64> = line
        .split_whitespace()
        .skip(1) // "cpu"
        .filter_map(|f| f.parse().ok())
        .collect();
    if fields.len() < 4 {
        return 0.0;
    }
    // user+nice+system = busy; +idle (+iowait not counted as idle
    // busy — iowait means the core was free but waiting on disk).
    let idle = fields[3] + fields.get(4).copied().unwrap_or(0);
    let total: u64 = fields.iter().sum();
    let frac = match *prev {
        Some((pi, pt)) if total > pt => {
            let busy = (total - pt).saturating_sub(idle.saturating_sub(pi));
            (busy as f32 / (total - pt) as f32).clamp(0.0, 1.0)
        }
        _ => 0.0,
    };
    *prev = Some((idle, total));
    frac
}

/// The overlay: a bordered box pinned to the top-right of `area`,
/// traced with the current oscillator frame.
pub fn draw(f: &mut Frame, area: Rect, input: ScopeInput, phase: f32) {
    const W: u16 = 24;
    const H: u16 = 11;
    if area.width < W + 2 || area.height < H + 2 {
        return;
    }
    let rect = Rect {
        x: area.x + area.width - W,
        y: area.y,
        width: W,
        height: H,
    };
    f.render_widget(Clear, rect);

    let block = Block::default()
        .title(" scope ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(FAINT))
        .style(Style::default().bg(BG));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let (wc, hc) = (inner.width as usize, inner.height as usize);
    if wc == 0 || hc == 0 {
        return;
    }
    let mut dots = vec![0u8; wc * hc];
    trace(&mut dots, wc, hc, input, phase);

    let buf = f.buffer_mut();
    for cy in 0..hc {
        for cx in 0..wc {
            let bits = dots[cy * wc + cx];
            if bits == 0 {
                continue;
            }
            let cell = &mut buf[(inner.x + cx as u16, inner.y + cy as u16)];
            cell.set_char(char::from_u32(0x2800 + bits as u32).unwrap_or('⠿'));
            // Hotter trace when the model is fast, dim when idle.
            cell.set_style(Style::default().fg(if input.tps > 1.0 {
                ORANGE
            } else {
                ORANGE_DIM
            }));
        }
    }

    // One-line readout under the ring: raw inputs for the curious.
    if inner.height > 0 {
        let label = format!(
            "{:>4.0}t/s mem{:>3.0}% cpu{:>3.0}%",
            input.tps,
            input.mem * 100.0,
            input.cpu * 100.0
        );
        let y = inner.y + inner.height - 1;
        for (i, c) in label.chars().take(inner.width as usize).enumerate() {
            let cell = &mut buf[(inner.x + i as u16, y)];
            cell.set_char(c);
            cell.set_style(Style::default().fg(FAINT));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_step_grows_with_tps() {
        assert!(phase_step(50.0) > phase_step(0.0));
        assert!(phase_step(10_000.0) <= phase_step(120.0) + f32::EPSILON);
    }

    #[test]
    fn trace_stays_in_bounds() {
        let (w, h) = (20, 8);
        let mut dots = vec![0u8; w * h];
        trace(&mut dots, w, h, ScopeInput { tps: 30.0, mem: 1.0, cpu: 1.0 }, 1.0);
        // Some dots lit, none out of the allocated buffer by construction.
        assert!(dots.iter().any(|&d| d != 0));
    }

    #[test]
    fn mem_pressure_parses() {
        // Real host: must be in (0, 1).
        let p = mem_pressure();
        assert!(p > 0.0 && p <= 1.0);
    }

    #[test]
    fn cpu_busy_first_sample_zero_then_real() {
        let mut prev = None;
        assert_eq!(cpu_busy(&mut prev), 0.0);
        std::thread::sleep(std::time::Duration::from_millis(60));
        let v = cpu_busy(&mut prev);
        assert!((0.0..=1.0).contains(&v));
    }
}
