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
use std::io::{self, Write};
use std::process::{Command, Stdio};
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
        if let Some(source) = session.take_copy_request() {
            match copy_source(&source) {
                Ok(message) => session.set_message(message),
                Err(error) => session.set_message(format!("Copy failed: {error}")),
            }
        }
    }
}

fn copy_source(source: &str) -> io::Result<&'static str> {
    let remote =
        std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some();
    if cfg!(target_os = "macos") && !remote {
        let mut child = Command::new("/usr/bin/pbcopy")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        let written = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("pbcopy stdin unavailable"))?
            .write_all(source.as_bytes());
        let output = child.wait_with_output()?;
        written?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "pbcopy exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim(),
            )));
        }
        return Ok("Copied source to clipboard");
    }
    let term = std::env::var("TERM").unwrap_or_default();
    let mux = if std::env::var_os("TMUX").is_some() || term.starts_with("tmux") {
        ClipboardMux::Tmux
    } else if std::env::var_os("STY").is_some() || term.starts_with("screen") {
        ClipboardMux::Screen
    } else {
        ClipboardMux::None
    };
    let mut output = io::BufWriter::new(io::stdout().lock());
    write_osc52(&mut output, source.as_bytes(), mux)?;
    output.flush()?;
    Ok("Copy sent via OSC52; terminal clipboard permission is required")
}

#[derive(Clone, Copy)]
enum ClipboardMux {
    None,
    Tmux,
    Screen,
}

fn write_osc52(output: &mut impl Write, source: &[u8], mux: ClipboardMux) -> io::Result<()> {
    match mux {
        ClipboardMux::None => output.write_all(b"\x1b]52;c;")?,
        ClipboardMux::Tmux => output.write_all(b"\x1bPtmux;\x1b\x1b]52;c;")?,
        ClipboardMux::Screen => output.write_all(b"\x1bP\x1b]52;c;")?,
    }
    const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for chunk in source.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        output.write_all(&[
            BASE64[usize::from(a >> 2)],
            BASE64[usize::from(((a & 3) << 4) | (b >> 4))],
            if chunk.len() > 1 {
                BASE64[usize::from(((b & 15) << 2) | (c >> 6))]
            } else {
                b'='
            },
            if chunk.len() > 2 {
                BASE64[usize::from(c & 63)]
            } else {
                b'='
            },
        ])?;
    }
    output.write_all(b"\x07")?;
    if !matches!(mux, ClipboardMux::None) {
        output.write_all(b"\x1b\\")?;
    }
    Ok(())
}

#[cfg(test)]
mod clipboard_tests {
    use super::*;

    #[test]
    fn osc52_encodes_source_bytes_and_base64_padding() {
        for (source, encoded) in [
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("\t界e\u{301}\n", "CeeVjGXMgQo="),
        ] {
            let mut output = Vec::new();
            write_osc52(&mut output, source.as_bytes(), ClipboardMux::None).unwrap();
            assert_eq!(output, format!("\x1b]52;c;{encoded}\x07").as_bytes());
        }
    }

    #[test]
    fn osc52_escapes_mux_passthrough_without_changing_clipboard_payload() {
        for (mux, expected) in [
            (
                ClipboardMux::Tmux,
                b"\x1bPtmux;\x1b\x1b]52;c;Zm9v\x07\x1b\\".as_slice(),
            ),
            (
                ClipboardMux::Screen,
                b"\x1bP\x1b]52;c;Zm9v\x07\x1b\\".as_slice(),
            ),
        ] {
            let mut output = Vec::new();
            write_osc52(&mut output, b"foo", mux).unwrap();
            assert_eq!(output, expected);
        }
    }
}
