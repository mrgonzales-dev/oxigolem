//! UI animation math — pure functions with no terminal needed.

use ratatui::style::Color;

#[test]
fn model_hue_drifts_over_time() {
    let a = oxigolem::ui::header::model_shimmer_hue(0, 0);
    let b = oxigolem::ui::header::model_shimmer_hue(0, 25);
    assert!(a != b, "hue must move with the tick");
}

#[test]
fn model_hue_varies_across_columns() {
    let a = oxigolem::ui::header::model_shimmer_hue(0, 0);
    let b = oxigolem::ui::header::model_shimmer_hue(10, 0);
    assert!(a != b, "adjacent chars must hue-offset — the shimmer is a wave");
}

#[test]
fn model_hue_wraps_cleanly() {
    // Hue never escapes -> 0..360. Wraparound must be continuous: 355°
    // and 5° are neighbors (10° apart), not 350° apart.
    for col in [0usize, 1, 7, 24] {
        for tick in [0u64, 1, 60, 183, 1000] {
            let h = oxigolem::ui::header::model_shimmer_hue(col, tick);
            assert!((0.0..360.0).contains(&h), "hue {h} out of range col={col} tick={tick}");
        }
    }
    let near = oxigolem::ui::header::model_shimmer_hue(0, 176); // ≈352°
    let after = oxigolem::ui::header::model_shimmer_hue(0, 181); // ≈2°
    let circ = (near - after).abs();
    let circ = circ.min(360.0 - circ); // circular distance across 0°
    assert!(circ < 20.0, "wraparound must stay continuous: {near} vs {after}");
}

#[test]
fn hsv_primary_hues() {
    // Saturated primaries land where you'd expect.
    let (Color::Rgb(r, g, b_a), Color::Rgb(r2, g2, b2), Color::Rgb(r3, g3, b3)) = (
        oxigolem::ui::header::hsv_to_rgb(0.0, 1.0, 1.0),
        oxigolem::ui::header::hsv_to_rgb(120.0, 1.0, 1.0),
        oxigolem::ui::header::hsv_to_rgb(240.0, 1.0, 1.0),
    ) else {
        panic!("hsv_to_rgb must return Rgb");
    };
    assert_eq!((r, g, b_a), (255, 0, 0), "hue 0 is red");
    assert_eq!((r2, g2, b2), (0, 255, 0), "hue 120 is green");
    assert_eq!((r3, g3, b3), (0, 0, 255), "hue 240 is blue");
}

#[test]
fn hsv_desaturated_is_gray() {
    for hue in [0.0, 37.0, 199.0, 359.0] {
        let Color::Rgb(r, g, b_a) = oxigolem::ui::header::hsv_to_rgb(hue, 0.0, 0.8) else {
            panic!("hsv_to_rgb must return Rgb");
        };
        assert_eq!(r, g, "s=0 → gray (hue {hue})");
        assert_eq!(g, b_a, "s=0 → gray (hue {hue})");
    }
}
