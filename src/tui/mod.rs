//! Interactive configurator: terminal lifecycle and the event loop. State and drawing live in
//! `state` / `view`; the preview reaches ratatui through `convert`, never by re-parsing ANSI.
//! Every exit path, including a panic, must hand the terminal back out of raw mode and the
//! alternate screen.

use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::{Backend, CrosstermBackend};

pub use self::convert::rendered_frame_to_ratatui_lines;
pub use self::state::{ConfiguratorApp, ConfiguratorEnv, ConfiguratorExit, TuiTab};

pub mod convert;
mod input;
mod state;
mod view;

/// Draws one workbench frame. Generic over the backend so tests can render the same UI through
/// `TestBackend` with no TTY.
pub fn render_tui_frame<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &ConfiguratorApp,
) -> io::Result<()> {
    terminal.draw(|frame| view::draw_workbench(frame, app))?;
    Ok(())
}

/// RAII terminal restore: a panic or an early `?` must still leave the user's shell usable.
struct TerminalCleanupGuard {
    // Makes restore idempotent: `run_configurator` calls it explicitly, then Drop calls it again.
    active: bool,
}

impl TerminalCleanupGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        // Raw mode is already on; a failed screen switch must not leave the shell without echo.
        if let Err(err) = execute!(stdout, EnterAlternateScreen, EnableMouseCapture) {
            let _ = disable_raw_mode();
            return Err(err);
        }
        Ok(Self { active: true })
    }

    fn restore(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let mut stdout = io::stdout();
        let _ = execute!(stdout, DisableMouseCapture, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

impl Drop for TerminalCleanupGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

/// Runs the workbench until the user quits. Files are written only by the app's save and hook
/// paths; this function owns the terminal and reports how the session ended.
pub fn run_configurator() -> io::Result<ConfiguratorExit> {
    let env = ConfiguratorEnv::from_process();
    let mut app = ConfiguratorApp::new(env);

    // Restore the terminal before the previous hook prints, so a panic report stays readable.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut out = io::stdout();
        let _ = execute!(out, DisableMouseCapture, LeaveAlternateScreen);
        let _ = disable_raw_mode();
        prev_hook(info);
    }));

    let mut guard = TerminalCleanupGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal: Terminal<CrosstermBackend<Stdout>> = Terminal::new(backend)?;

    // No Resize event arrives for the initial size; seed it or the first frame uses the default.
    if let Ok((w, h)) = crossterm::terminal::size() {
        app.handle_event(Event::Resize(w, h));
    }

    let loop_res = run_event_loop(&mut terminal, &mut app);
    guard.restore();
    loop_res
}

/// Draws before waiting so the first frame needs no input. Returns only on a `ConfiguratorExit`;
/// on an I/O error the cleanup guard's Drop still restores the terminal.
fn run_event_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut ConfiguratorApp,
) -> io::Result<ConfiguratorExit> {
    loop {
        render_tui_frame(terminal, app)?;
        if event::poll(Duration::from_millis(250))? {
            let ev = event::read()?;
            if let Some(exit) = app.handle_event(ev) {
                return Ok(exit);
            }
        }
    }
}
