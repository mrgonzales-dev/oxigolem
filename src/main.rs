use oxigolem::app::{App, View};
use oxigolem::ui;
use crossterm::{
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    Terminal,
};
use std::io;
use std::time::Duration;

fn main() -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = run(&mut terminal);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableBracketedPaste)?;
    res
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    let mut app = App::new();

    loop {
        terminal.draw(|f| draw(f, &app))?;

        app.poll_chat();

        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Paste(text) if app.modal.is_none() => app.push_paste(text),
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.open_palette()
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if app.ctrl_c() {
                            return Ok(());
                        }
                    }
                    code if app.modal.is_some() => app.modal_key(code),
                    KeyCode::BackTab => {
                        app.dismiss_toast();
                        app.toggle_view();
                    }
                    KeyCode::Esc => return Ok(()),
                    KeyCode::Enter => {
                        app.dismiss_toast();
                        app.handle_enter();
                    }
                    KeyCode::Backspace => {
                        app.dismiss_toast();
                        app.pop_input();
                    }
                    KeyCode::Char('q') if app.is_input_empty() => return Ok(()),
                    KeyCode::Char(c) => {
                        app.dismiss_toast();
                        app.push_char(c);
                    }
                    _ => {}
                },
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
            Constraint::Min(0),    // body
            Constraint::Length(ui::input::height_for(&app.input_display(), area.width)), // input (grows downward)
            Constraint::Length(2), // footer status bar
        ])
        .split(area);

    match app.view {
        View::Agent => ui::log::draw(f, root[0], &app.transcript),
        View::Output => ui::output::draw(f, root[0], &app.term_lines),
    }

    ui::input::draw(f, root[1], &app.input_display());
    ui::header::draw(f, root[2], app.view, app.connected.as_ref());

    if let Some(m) = &app.modal {
        ui::modal::draw(f, area, m, &app.modal_items());
    }

    if let Some(t) = &app.toast {
        ui::toast::draw(f, area, t);
    }
}
