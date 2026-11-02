mod app;
mod theme;
mod ui;

use app::{App, View};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    Terminal,
};
use std::io;

fn main() -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = run(&mut terminal);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    res
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    let mut app = App::new();

    loop {
        terminal.draw(|f| draw(f, &app))?;

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::BackTab => app.toggle_view(),
                KeyCode::Esc => return Ok(()),
                KeyCode::Enter => app.run_command(),
                KeyCode::Backspace => {
                    app.input.pop();
                }
                KeyCode::Char('q') if app.input.is_empty() => return Ok(()),
                KeyCode::Char(c) => app.input.push(c),
                _ => {}
            }
        }
    }
}

fn draw(f: &mut ratatui::Frame, app: &App) {
    let area = f.area();

    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Min(0),    // body
            Constraint::Length(3), // input
        ])
        .split(area);

    ui::header::draw(f, root[0], app.view);

    match app.view {
        View::Agent => ui::log::draw(f, root[1]),
        View::Output => ui::output::draw(f, root[1], &app.term_lines),
    }

    ui::input::draw(f, root[2], &app.input);
}
