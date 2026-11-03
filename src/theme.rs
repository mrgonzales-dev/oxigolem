use ratatui::style::Color;

// Rust-orange on dim black
pub const ORANGE: Color = Color::Rgb(222, 88, 51); // rust orange
pub const ORANGE_DIM: Color = Color::Rgb(140, 60, 35);
pub const FAINT: Color = Color::Rgb(90, 84, 78); // dim gray for borders/meta
pub const BG: Color = Color::Rgb(10, 8, 7); // near-black warm
pub const TEXT: Color = Color::Rgb(210, 200, 190); // warm off-white
pub const USER_BG: Color = Color::Rgb(40, 36, 33); // gray highlight for user lines
pub const THINK_HOT: Color = Color::Rgb(255, 214, 170); // near-white orange peak of the thinking shimmer
pub const THINK_BASE: Color = Color::Rgb(222, 130, 80); // warm orange body of the shimmer
pub const LIVE_HOT: Color = Color::Rgb(220, 255, 220); // near-white green peak — live tool output
pub const LIVE_BASE: Color = Color::Rgb(120, 230, 140); // matrix green — live tool output
// Shell view accent — distinct from the agent's orange so a glance
// tells you which surface you're on.
pub const BLUE: Color = Color::Rgb(110, 170, 255);
