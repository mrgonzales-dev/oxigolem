use oxigolem::app::{App, View};
use oxigolem::ui;
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste,
        EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind,
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
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = run(&mut terminal);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableBracketedPaste,
        DisableMouseCapture
    )?;
    res
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    let mut app = App::new();

    loop {
        terminal.draw(|f| draw(f, &app))?;

        app.poll_chat();
        app.poll_shells();
        app.poll_models();
        app.normalize_view();
        app.tick = app.tick.wrapping_add(1);

        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Mouse(m) if app.modal.is_none() => match (app.view, m.kind) {
                    (View::Agent, MouseEventKind::ScrollUp) => app.scroll_up(3),
                    (View::Agent, MouseEventKind::ScrollDown) => app.scroll_down(3),
                    (View::Shell(_), MouseEventKind::ScrollUp) => app.term_scroll_up(3),
                    (View::Shell(_), MouseEventKind::ScrollDown) => app.term_scroll_down(3),
                    _ => {}
                },
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
                    KeyCode::Char('e')
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && app.view == View::Agent =>
                    {
                        app.toggle_tool_expand()
                    }
                    KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if app.ctrl_b() == "first" {
                            app.toast = Some("ctrl+b again to demote".to_string());
                        } else {
                            app.toast = Some("demoted to background".to_string());
                        }
                    }
                    code if app.modal.is_some() => app.modal_key(code),
                    KeyCode::PageUp => match app.view {
                        View::Agent => app.scroll_up(10),
                        View::Shell(_) => app.term_scroll_up(10),
                    },
                    KeyCode::PageDown => match app.view {
                        View::Agent => app.scroll_down(10),
                        View::Shell(_) => app.term_scroll_down(10),
                    },
                    KeyCode::Up => match app.view {
                        View::Agent => app.scroll_up(1),
                        View::Shell(_) => app.term_scroll_up(1),
                    },
                    KeyCode::Down => match app.view {
                        View::Agent => app.scroll_down(1),
                        View::Shell(_) => app.term_scroll_down(1),
                    },
                    KeyCode::BackTab => {
                        app.dismiss_toast();
                        app.next_view();
                    }
                    KeyCode::Esc => {
                        if app.esc() {
                            return Ok(());
                        }
                    }
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
            Constraint::Length(ui::input::height_for(
                &app.input_display(),
                area.width,
            )), // input (grows downward)
            Constraint::Length(2), // footer status bar
        ])
        .split(area);

    // Inset the body by one row at the bottom so the transcript's last
    // visible line never hugs the input's top border — reads as a
    // gutter between stream and prompt.
    let body = ratatui::layout::Rect {
        height: root[0].height.saturating_sub(1),
        ..root[0]
    };
    match app.view {
        View::Agent => ui::log::draw(
            f,
            body,
            &app.transcript,
            app.chat_busy(),
            app.tick,
            app.scroll_back,
            app.gen_done_tick,
            &app.shells,
            &app.last_log_bottom,
        ),
        View::Shell(i) => {
            let reg = app.shells.lock().unwrap();
            if let Some(sh) = reg.get(i) {
                ui::output::draw(f, body, sh, app.term_scroll, &app.last_term_bottom);
            }
        }
    }

    ui::input::draw(
        f,
        root[1],
        &app.input_display(),
        app.tick,
        App::slash_cmd_prefix_len(&app.input_display()),
    );
    ui::header::draw(
        f,
        root[2],
        app.view,
        app.connected.as_ref(),
        &app.shells,
        app.last_usage.as_ref(),
        ui::header::ChipState {
            busy: app.chat_busy(),
            connecting: app.connecting(),
            tick: app.tick,
        },
    );

    if let Some(m) = &app.modal {
        ui::modal::draw(f, area, m, &app.modal_items());
    }

    if let Some(t) = &app.toast {
        ui::toast::draw(f, area, t);
    }
}
