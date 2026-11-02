use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem},
    Frame,
};

use crate::theme::{FAINT, ORANGE, ORANGE_DIM, TEXT};

pub fn draw(f: &mut Frame, area: Rect) {
    let entries = vec![
        ("user", "list the files in this directory", FAINT),
        ("agent", "$ ls -la", ORANGE),
        ("tool", "returned 12 entries", ORANGE_DIM),
        ("agent", "$ cat Cargo.toml", ORANGE),
        ("tool", "returned 24 lines", ORANGE_DIM),
        ("agent", "The project is a Rust binary with ratatui and crossterm.", TEXT),
    ];

    let items: Vec<ListItem> = entries
        .iter()
        .map(|(role, text, color)| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{role:>6} "),
                    Style::default().fg(*color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(*text, Style::default().fg(TEXT)),
            ]))
        })
        .collect();

    let log = List::new(items).block(
        Block::default()
            .title(" agent ")
            .title_style(Style::default().fg(ORANGE))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(FAINT)),
    );
    f.render_widget(log, area);
}
