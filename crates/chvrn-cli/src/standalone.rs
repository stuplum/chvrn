use crate::files::{GuardedFile, read_bytes};
use crate::jev_ui::JevUi;
use crate::language::LanguageUi;
use crate::terminal::{self, ReviewHost};
use crate::watch::{BackgroundDiff, FileWatch};
use crate::{MergeArgs, Options, OutputFormat, Result};
use chvrn_core::TextSnapshot;
use chvrn_core::diff::{Diff, WhitespacePolicy};
use chvrn_core::merge::Merge;
use chvrn_core::structural::{Language, StructuralAnalysis};
use chvrn_tui::{DiffRequestId, Pane, ReviewInput, ReviewOutcome, ReviewSession, ReviewSubmission};
use crossterm::event::{Event, KeyCode};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub fn snapshot(bytes: &[u8]) -> Result<TextSnapshot> {
    TextSnapshot::from_bytes(bytes)
        .map_err(|error| format!("input is not editable UTF-8 text: {error:?}").into())
}

pub fn diff_value(left: &[u8], right: &[u8], path: &Path, policy: WhitespacePolicy) -> Value {
    let (Ok(left), Ok(right)) = (
        TextSnapshot::from_bytes(left),
        TextSnapshot::from_bytes(right),
    ) else {
        return json!({"equal": left == right, "binary": true, "hunks": []});
    };
    let diff = Diff::between(&left, &right, policy);
    let hunks: Vec<_> = diff
        .hunks()
        .iter()
        .map(|hunk| {
            json!({
                "left": {"start": hunk.left_lines.start, "end": hunk.left_lines.end},
                "right": {"start": hunk.right_lines.start, "end": hunk.right_lines.end},
            })
        })
        .collect();
    let structural = match StructuralAnalysis::compare(Language::for_path(path), &left, &right) {
        Ok(analysis) => {
            json!({"available": true, "changes": analysis.changes().iter().map(|change| json!({
            "kind": format!("{:?}", change.kind),
            "before": {"start": change.before.start, "end": change.before.end},
            "after": {"start": change.after.start, "end": change.after.end},
        })).collect::<Vec<_>>()})
        }
        Err(error) => json!({"available": false, "reason": format!("{error:?}")}),
    };
    json!({"equal": hunks.is_empty(), "binary": false, "hunks": hunks, "structural": structural})
}

pub fn print_value(value: &Value, format: OutputFormat) -> Result<()> {
    if format == OutputFormat::Text {
        if let Some(files) = value["files"].as_array() {
            for file in files {
                println!(
                    "{} {}",
                    if file["equal"] == true { "=" } else { "!" },
                    file["path"]
                );
            }
        } else {
            println!(
                "{}; {} hunks",
                if value["equal"] == true {
                    "equal"
                } else {
                    "different"
                },
                value["hunks"].as_array().map_or(0, Vec::len)
            );
        }
    } else {
        println!("{}", serde_json::to_string(value)?);
    }
    Ok(())
}

pub fn diff(left: PathBuf, right: PathBuf, options: &Options) -> Result<u8> {
    if left == Path::new("-") && right == Path::new("-") {
        return Err("only one comparison input may read stdin".into());
    }
    let left_bytes = read_bytes(&left)?;
    let right_bytes = read_bytes(&right)?;
    if !options.interactive() || left == Path::new("-") || right == Path::new("-") {
        let value = diff_value(&left_bytes, &right_bytes, &right, options.whitespace.into());
        let code = u8::from(value["equal"] != true);
        print_value(&value, options.format)?;
        return Ok(code);
    }
    let left_file = GuardedFile::read(&left)?;
    let right_file = GuardedFile::read(&right)?;
    if left_file.path == right_file.path {
        return Err("editable comparison requires two distinct files".into());
    }
    let left_text = snapshot(left_file.bytes())?;
    let right_text = snapshot(right_file.bytes())?;
    let mut session = ReviewSession::two_way(left_text.text(), right_text.text());
    session.set_theme(std::sync::Arc::clone(&options.loaded_theme));
    session.set_paths(&left_file.path, &right_file.path);
    session.set_whitespace_policy(options.whitespace.into());
    let watch = FileWatch::new(&[&left_file.path, &right_file.path], false)?;
    let language = LanguageUi::new(
        options,
        left_file.path.parent().ok_or("input has no parent")?,
    )?;
    let mut host = FileHost {
        left: left_file,
        right: right_file,
        watch,
        language,
        background: BackgroundDiff::new(),
        reading: None,
        refresh_again: false,
        pending: None,
        pending_request: None,
        refresh_conflict: false,
    };
    terminal::run(&mut session, &mut host)
}

struct FileRefresh {
    left: GuardedFile,
    right: GuardedFile,
    left_text: TextSnapshot,
    right_text: TextSnapshot,
}

struct FileHost {
    left: GuardedFile,
    right: GuardedFile,
    watch: FileWatch,
    language: LanguageUi,
    background: BackgroundDiff,
    reading: Option<std::sync::mpsc::Receiver<Result<Option<FileRefresh>>>>,
    refresh_again: bool,
    pending: Option<(GuardedFile, GuardedFile)>,
    pending_request: Option<DiffRequestId>,
    refresh_conflict: bool,
}

impl FileHost {
    fn retire_refresh(&mut self) {
        self.pending = None;
        self.pending_request = None;
        self.reading = None;
        self.refresh_conflict = false;
        self.refresh_again = true;
    }
}

impl ReviewHost for FileHost {
    fn tick(&mut self, session: &mut ReviewSession) -> Result<()> {
        let path = if session.focus() == Pane::Left {
            &self.left.path
        } else {
            &self.right.path
        };
        self.language.tick(session, path)?;
        if self.language.viewing_definition() {
            return Ok(());
        }
        self.refresh_again |= self.watch.changed()?;
        if let Some(reading) = &self.reading {
            if let Ok(result) = reading.try_recv() {
                self.reading = None;
                if let Some(FileRefresh {
                    left,
                    right,
                    left_text,
                    right_text,
                }) = result?
                {
                    let request = session.request_diff_snapshots(left_text, right_text);
                    self.pending_request = Some(request.id());
                    self.refresh_conflict = false;
                    self.background.request(request);
                    self.pending = Some((left, right));
                }
            }
        }
        if self.refresh_again && self.reading.is_none() {
            self.refresh_again = false;
            let previous_left = self.left.clone();
            let previous_right = self.right.clone();
            let (send, receive) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let result = (|| {
                    let left = GuardedFile::read(&previous_left.path)?;
                    let right = GuardedFile::read(&previous_right.path)?;
                    if left.same_state(&previous_left) && right.same_state(&previous_right) {
                        return Ok(None);
                    }
                    let left_text = snapshot(left.bytes())?;
                    let right_text = snapshot(right.bytes())?;
                    Ok(Some(FileRefresh {
                        left,
                        right,
                        left_text,
                        right_text,
                    }))
                })();
                let _ = send.send(result);
            });
            self.reading = Some(receive);
        }
        if let Some(completion) = self.background.latest() {
            if self.pending_request == Some(completion.id()) {
                match session.handle(ReviewInput::DiffReady(completion)) {
                    ReviewOutcome::RefreshConflict => {
                        self.refresh_conflict = true;
                        session.set_message(
                            "Files changed externally. R explicitly discards local edits and reloads",
                        );
                    }
                    ReviewOutcome::RefreshApplied => {
                        self.refresh_conflict = false;
                        self.pending_request = None;
                        if let Some((left, right)) = self.pending.take() {
                            self.left = left;
                            self.right = right;
                        }
                    }
                    _ => self.retire_refresh(),
                }
            }
        }
        Ok(())
    }

    fn input(&mut self, session: &mut ReviewSession, event: &Event) -> Result<bool> {
        let path = if session.focus() == Pane::Left {
            &self.left.path
        } else {
            &self.right.path
        };
        if self.language.input(session, path, event)? {
            return Ok(true);
        }
        if matches!(event, Event::Key(key) if key.code == KeyCode::Char('R'))
            && !session.is_editing()
            && self.refresh_conflict
        {
            if session.handle(ReviewInput::DiscardAndReload) != ReviewOutcome::RefreshApplied {
                self.retire_refresh();
                return Err(
                    "the latest refresh is still computing; no filesystem guards were changed"
                        .into(),
                );
            }
            if let Some((left, right)) = self.pending.take() {
                self.pending_request = None;
                self.refresh_conflict = false;
                self.left = left;
                self.right = right;
                session.set_message("External snapshot reloaded after explicit discard");
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn submit(
        &mut self,
        _session: &mut ReviewSession,
        submission: ReviewSubmission,
    ) -> Result<bool> {
        self.left.validate()?;
        self.right.validate()?;
        let mut written = Vec::new();
        for (file, bytes) in [
            (&mut self.left, submission.left.as_bytes()),
            (&mut self.right, submission.right.as_bytes()),
        ] {
            if file.bytes() != bytes {
                if let Err(error) = file.write(bytes) {
                    return Err(format!(
                        "save failed: {error}; already written files: {written:?}"
                    )
                    .into());
                }
                written.push(file.path.clone());
            }
        }
        Ok(true)
    }
}

pub fn merge(args: MergeArgs, options: &Options) -> Result<u8> {
    let jev = JevUi::from_options(options)?;
    let base = GuardedFile::read(&args.base)?;
    let ours = GuardedFile::read(&args.ours)?;
    let theirs = GuardedFile::read(&args.theirs)?;
    let mut output = GuardedFile::capture(&args.output)?;
    let base_text = snapshot(base.bytes())?;
    let ours_text = snapshot(ours.bytes())?;
    let theirs_text = snapshot(theirs.bytes())?;
    if !options.interactive() {
        let merge = Merge::three_way(&base_text, &ours_text, &theirs_text);
        let Some(result) = merge.result() else {
            eprintln!(
                "chvrn: {} unresolved merge conflicts; output unchanged",
                merge.conflicts().len()
            );
            return Ok(1);
        };
        base.validate()?;
        ours.validate()?;
        theirs.validate()?;
        output.write(result.as_bytes())?;
        return Ok(0);
    }
    let mut session =
        ReviewSession::three_way(base_text.text(), ours_text.text(), theirs_text.text());
    session.set_theme(std::sync::Arc::clone(&options.loaded_theme));
    session.set_paths(&ours.path, &theirs.path);
    session.set_read_only(Pane::Ours, true);
    session.set_read_only(Pane::Theirs, true);
    session.set_output_path(&output.path);
    session.set_merge_advice_enabled(jev.is_some());
    let language = LanguageUi::new(options, output.path.parent().ok_or("output has no parent")?)?;
    let mut host = MergeHost {
        base,
        ours,
        theirs,
        output,
        language,
        jev,
    };
    terminal::run(&mut session, &mut host)
}

struct MergeHost {
    base: GuardedFile,
    ours: GuardedFile,
    theirs: GuardedFile,
    output: GuardedFile,
    language: LanguageUi,
    jev: Option<JevUi>,
}

impl ReviewHost for MergeHost {
    fn tick(&mut self, session: &mut ReviewSession) -> Result<()> {
        let path = match session.focus() {
            Pane::Ours => &self.ours.path,
            Pane::Theirs => &self.theirs.path,
            _ => &self.output.path,
        };
        self.language.tick(session, path)?;
        if let Some(jev) = &mut self.jev {
            jev.tick(session);
        }
        Ok(())
    }

    fn input(&mut self, session: &mut ReviewSession, event: &Event) -> Result<bool> {
        if !self.language.viewing_definition() {
            if let Some(jev) = &mut self.jev {
                if jev.input(session, event) {
                    return Ok(true);
                }
            }
        }
        let path = match session.focus() {
            Pane::Ours => &self.ours.path,
            Pane::Theirs => &self.theirs.path,
            _ => &self.output.path,
        };
        self.language.input(session, path, event)
    }

    fn submit(
        &mut self,
        _session: &mut ReviewSession,
        submission: ReviewSubmission,
    ) -> Result<bool> {
        let result = submission.result.ok_or("merge submission has no result")?;
        self.base.validate()?;
        self.ours.validate()?;
        self.theirs.validate()?;
        self.output.write(result.as_bytes())?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use crossterm::event::{KeyEvent, KeyModifiers};
    use std::time::{Duration, Instant};

    fn with_file_host(check: impl FnOnce(&mut FileHost, &mut ReviewSession)) {
        let directory = tempfile::tempdir().unwrap();
        let left = directory.path().join("left");
        let right = directory.path().join("right");
        std::fs::write(&left, "left\n").unwrap();
        std::fs::write(&right, "old\n").unwrap();
        let options = crate::Cli::parse_from(["chvrn", "diff", "left", "right"]).options;
        let mut host = FileHost {
            left: GuardedFile::read(&left).unwrap(),
            right: GuardedFile::read(&right).unwrap(),
            watch: FileWatch::new(&[], false).unwrap(),
            language: LanguageUi::new(&options, directory.path()).unwrap(),
            background: BackgroundDiff::new(),
            reading: None,
            refresh_again: false,
            pending: None,
            pending_request: None,
            refresh_conflict: false,
        };
        let mut session = ReviewSession::two_way("left\n", "old\n");
        check(&mut host, &mut session);
    }

    fn queue_file_refresh(host: &mut FileHost, session: &mut ReviewSession) {
        std::fs::write(&host.right.path, "external\n").unwrap();
        let left = GuardedFile::read(&host.left.path).unwrap();
        let right = GuardedFile::read(&host.right.path).unwrap();
        let request = session.request_diff_snapshots(
            snapshot(left.bytes()).unwrap(),
            snapshot(right.bytes()).unwrap(),
        );
        host.pending_request = Some(request.id());
        host.pending = Some((left, right));
        host.background.request(request);
    }

    fn finish_file_refresh(host: &mut FileHost, session: &mut ReviewSession) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while host.pending.is_some() && !host.refresh_conflict {
            host.tick(session).unwrap();
            assert!(Instant::now() < deadline, "file refresh did not finish");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn whitespace_change_publishes_matching_file_guards() {
        with_file_host(|host, session| {
            queue_file_refresh(host, session);
            session.set_whitespace_policy(WhitespacePolicy::IgnoreEdge);
            finish_file_refresh(host, session);
            assert_eq!(session.pane_text(Pane::Right), "external\n");
            assert_eq!(host.right.bytes(), b"external\n");
            host.right.validate().unwrap();
        });
    }

    #[test]
    fn dirty_policy_change_keeps_original_guards_until_explicit_discard() {
        with_file_host(|host, session| {
            queue_file_refresh(host, session);
            session.go_to(Pane::Right, 0, 0);
            session.handle(ReviewInput::Key(KeyEvent::new(
                KeyCode::Char('i'),
                KeyModifiers::NONE,
            )));
            session.handle(ReviewInput::Paste("edit".into()));
            session.handle(ReviewInput::Key(KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            )));
            session.set_whitespace_policy(WhitespacePolicy::IgnoreEdge);
            finish_file_refresh(host, session);
            assert!(host.refresh_conflict);
            assert_eq!(session.pane_text(Pane::Right), "editold\n");
            assert_eq!(host.right.bytes(), b"old\n");
            assert!(host.right.validate().is_err());
            assert!(
                host.input(
                    session,
                    &Event::Key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE))
                )
                .unwrap()
            );
            assert_eq!(session.pane_text(Pane::Right), "external\n");
            assert_eq!(host.right.bytes(), b"external\n");
            host.right.validate().unwrap();
        });
    }

    #[test]
    fn foreign_completion_cannot_publish_file_guards_for_an_accepted_generation() {
        with_file_host(|host, session| {
            queue_file_refresh(host, session);
            *session = ReviewSession::two_way("left\n", "old\n");
            let request = session.request_diff("left\n", "other\n");
            assert_eq!(
                session.handle(ReviewInput::DiffReady(request.compute())),
                ReviewOutcome::RefreshApplied
            );
            finish_file_refresh(host, session);
            assert_eq!(session.pane_text(Pane::Right), "other\n");
            assert_eq!(host.right.bytes(), b"old\n");
            assert!(host.right.validate().is_err());
            assert!(host.refresh_again);
        });
    }
}
