use crate::Result;
use chvrn_tui::{ReviewInput, ReviewOutcome, ReviewSession, ReviewSubmission};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io;
use std::time::Duration;

pub trait ReviewHost {
    fn tick(&mut self, _session: &mut ReviewSession) -> Result<()> {
        Ok(())
    }
    fn finished(&self) -> Option<u8> {
        None
    }
    fn input(&mut self, _session: &mut ReviewSession, _event: &Event) -> Result<bool> {
        Ok(false)
    }
    fn submit(&mut self, session: &mut ReviewSession, submission: ReviewSubmission)
    -> Result<bool>;
}

#[derive(Default)]
struct TerminalGuard {
    raw: bool,
    alternate: bool,
    mouse: bool,
    paste: bool,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        let mut guard = Self::default();
        enable_raw_mode()?;
        guard.raw = true;
        execute!(io::stdout(), EnterAlternateScreen)?;
        guard.alternate = true;
        execute!(io::stdout(), EnableMouseCapture)?;
        guard.mouse = true;
        execute!(io::stdout(), EnableBracketedPaste)?;
        guard.paste = true;
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.paste {
            let _ = execute!(io::stdout(), DisableBracketedPaste);
        }
        if self.mouse {
            let _ = execute!(io::stdout(), DisableMouseCapture);
        }
        if self.alternate {
            let _ = execute!(io::stdout(), crossterm::cursor::Show);
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
        }
        if self.raw {
            let _ = disable_raw_mode();
        }
    }
}

pub fn run(session: &mut ReviewSession, host: &mut impl ReviewHost) -> Result<u8> {
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    let mut discard = false;
    loop {
        session.poll_background();
        if let Err(error) = host.tick(session) {
            session.set_message(error.to_string());
        }
        if let Some(code) = host.finished() {
            return Ok(code);
        }
        terminal.draw(|frame| {
            let area = frame.area();
            session.handle(ReviewInput::Resize {
                width: area.width,
                height: area.height,
            });
            session.render(frame);
        })?;
        if !event::poll(Duration::from_millis(40))? {
            continue;
        }
        let event = event::read()?;
        if let Event::Key(key) = event {
            if key.kind == KeyEventKind::Release {
                continue;
            }
            if discard {
                discard = false;
                if key.code == KeyCode::Char('y') {
                    if session.handle(ReviewInput::ConfirmDiscard) == ReviewOutcome::Quit {
                        return Ok(1);
                    }
                } else {
                    session.handle(ReviewInput::Key(crossterm::event::KeyEvent::from(
                        KeyCode::Esc,
                    )));
                }
                continue;
            }
        }
        if !session.is_review_modal() {
            match host.input(session, &event) {
                Ok(true) => continue,
                Err(error) => {
                    session.set_message(error.to_string());
                    continue;
                }
                Ok(false) => {}
            }
        }
        let input = match event {
            Event::Key(key) => ReviewInput::Key(key),
            Event::Resize(width, height) => ReviewInput::Resize { width, height },
            Event::Mouse(mouse) => ReviewInput::Mouse(mouse),
            Event::Paste(text) => ReviewInput::Paste(text),
            _ => continue,
        };
        match session.handle(input) {
            ReviewOutcome::Quit => return Ok(1),
            ReviewOutcome::Submitted(submission) => match host.submit(session, submission) {
                Ok(true) => return Ok(0),
                Ok(false) => {}
                Err(error) => session.set_message(error.to_string()),
            },
            ReviewOutcome::DiscardRequired => {
                discard = true;
                session.set_message(
                    "Discard unsaved edits and quit? y discards; any other key keeps editing",
                );
            }
            ReviewOutcome::RefreshConflict => {
                session.set_message("Files changed externally. R discards local edits and reloads; submission is blocked");
            }
            ReviewOutcome::LocalDiffPending => {
                session.set_message("Waiting for the latest local diff before submitting");
            }
            ReviewOutcome::UnresolvedConflicts(count) => session.set_message(format!(
                "Resolve {count} remaining conflicts before submitting"
            )),
            ReviewOutcome::Continue => {}
        }
    }
}

pub fn run_pager(session: &mut chvrn_tui::PagerSession) -> Result<u8> {
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    loop {
        terminal.draw(|frame| session.render(frame))?;
        if session.handle(event::read()?) {
            return Ok(0);
        }
    }
}
