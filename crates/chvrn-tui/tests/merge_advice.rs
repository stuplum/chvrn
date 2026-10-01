use chvrn_core::merge_advice::{MergeAdviceChoice, MergeAdviceSuggestion};
use chvrn_tui::{MergeAdviceError, Pane, ReviewInput, ReviewOutcome, ReviewSession};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

const BASE: &str = "head\nbase-one\nkeep\nstay\nbase-two\ntail\n";
const OURS: &str = "head\nours-one\nkeep\nstay\nours-two\ntail\n";
const THEIRS: &str = "head\ntheirs-one\nkeep\nstay\ntheirs-two\ntail\n";

fn key(session: &mut ReviewSession, code: KeyCode) -> ReviewOutcome {
    session.handle(ReviewInput::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

fn ctrl_r(session: &mut ReviewSession) -> ReviewOutcome {
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Char('r'),
        KeyModifiers::CONTROL,
    )))
}

fn merge() -> ReviewSession {
    let mut session = ReviewSession::three_way(BASE, OURS, THEIRS);
    assert_eq!(session.unresolved_conflicts(), 2);
    assert_eq!(session.selected_hunk(), Some(0));
    session.set_merge_advice_enabled(true);
    session
}

fn suggestion(choice: MergeAdviceChoice) -> MergeAdviceSuggestion {
    MergeAdviceSuggestion {
        choice,
        confidence: 0.73,
        model: "jev-1.13.0".to_owned(),
    }
}

fn draw(session: &ReviewSession, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| session.render(frame)).unwrap();
    terminal.backend().buffer().clone()
}

fn visible_text(buffer: &Buffer) -> String {
    buffer.content.iter().map(|cell| cell.symbol()).collect()
}

#[test]
fn assistance_is_disabled_until_explicitly_enabled() {
    let mut session = ReviewSession::three_way(BASE, OURS, THEIRS);
    assert!(matches!(
        session.begin_merge_advice(),
        Err(MergeAdviceError::Disabled)
    ));
    assert_eq!(
        key(&mut session, KeyCode::Char('J')),
        ReviewOutcome::Continue
    );
    assert!(matches!(
        session.begin_merge_advice(),
        Err(MergeAdviceError::Disabled)
    ));
    assert_eq!(session.pane_text(Pane::Result), OURS);
    assert_eq!(session.unresolved_conflicts(), 2);
    assert!(!session.is_dirty());

    session.set_merge_advice_enabled(true);
    let request = session.begin_merge_advice().unwrap();
    assert!(session.receive_merge_advice(request, Ok(suggestion(MergeAdviceChoice::Theirs))));
    assert_eq!(key(&mut session, KeyCode::Enter), ReviewOutcome::Continue);
    assert_eq!(
        session.pane_text(Pane::Result),
        "head\ntheirs-one\nkeep\nstay\nours-two\ntail\n"
    );
    assert_eq!(session.unresolved_conflicts(), 1);
}

#[test]
fn non_merge_and_fully_resolved_sessions_cannot_request_advice() {
    let mut sessions = [
        ReviewSession::two_way("left\n", "right\n"),
        ReviewSession::three_way("same\n", "same\n", "same\n"),
    ];
    for session in &mut sessions {
        session.set_merge_advice_enabled(true);
        assert!(matches!(
            session.begin_merge_advice(),
            Err(MergeAdviceError::Unavailable)
        ));
        assert!(!session.is_dirty());
    }

    let mut session = ReviewSession::three_way("base\n", "ours\n", "theirs\n");
    session.set_merge_advice_enabled(true);
    key(&mut session, KeyCode::Char('o'));
    assert!(session.is_confirming_merge());
    assert!(matches!(
        session.begin_merge_advice(),
        Err(MergeAdviceError::Unavailable)
    ));
    key(&mut session, KeyCode::Esc);
    assert!(matches!(
        session.begin_merge_advice(),
        Err(MergeAdviceError::Unavailable)
    ));
}

#[test]
fn help_and_insert_mode_block_requests_without_poisoning_later_requests() {
    for modal in [KeyCode::Char('?'), KeyCode::Char('i')] {
        let mut session = merge();
        key(&mut session, modal);
        assert!(matches!(
            session.begin_merge_advice(),
            Err(MergeAdviceError::Unavailable)
        ));
        key(&mut session, KeyCode::Esc);
        let request = session.begin_merge_advice().unwrap();
        assert!(session.receive_merge_advice(request, Ok(suggestion(MergeAdviceChoice::Theirs))));
        key(&mut session, KeyCode::Enter);
        assert_eq!(session.unresolved_conflicts(), 1);
        assert_eq!(
            session.pane_text(Pane::Result),
            "head\ntheirs-one\nkeep\nstay\nours-two\ntail\n"
        );
    }
}

#[test]
fn edited_conflicts_are_unavailable_but_other_conflicts_remain_eligible() {
    let mut session = merge();
    session.go_to(Pane::Result, 1, 0);
    key(&mut session, KeyCode::Char('i'));
    key(&mut session, KeyCode::Char('X'));
    key(&mut session, KeyCode::Esc);
    assert!(matches!(
        session.begin_merge_advice(),
        Err(MergeAdviceError::Unavailable)
    ));
    assert_eq!(session.unresolved_conflicts(), 2);

    key(&mut session, KeyCode::Char(']'));
    let request = session.begin_merge_advice().unwrap();
    assert!(session.receive_merge_advice(request, Ok(suggestion(MergeAdviceChoice::Theirs))));
    key(&mut session, KeyCode::Enter);
    assert_eq!(
        session.pane_text(Pane::Result),
        "head\nXours-one\nkeep\nstay\ntheirs-two\ntail\n"
    );
    assert_eq!(session.unresolved_conflicts(), 1);
}

#[test]
fn discard_confirmation_blocks_advice_even_for_an_unedited_conflict() {
    let mut session = merge();
    session.go_to(Pane::Result, 0, 0);
    key(&mut session, KeyCode::Char('i'));
    key(&mut session, KeyCode::Char('X'));
    key(&mut session, KeyCode::Esc);
    assert_eq!(
        key(&mut session, KeyCode::Char('q')),
        ReviewOutcome::DiscardRequired
    );
    assert!(matches!(
        session.begin_merge_advice(),
        Err(MergeAdviceError::Unavailable)
    ));
    key(&mut session, KeyCode::Esc);
    let request = session.begin_merge_advice().unwrap();
    assert!(session.receive_merge_advice(request, Ok(suggestion(MergeAdviceChoice::Theirs))));
    key(&mut session, KeyCode::Enter);
    assert_eq!(
        session.pane_text(Pane::Result),
        "Xhead\ntheirs-one\nkeep\nstay\nours-two\ntail\n"
    );
}

#[test]
fn pending_local_alignment_blocks_advice() {
    let prefix: String = (0..20_000).map(|line| format!("line{line:05}\n")).collect();
    let mut session = ReviewSession::three_way(
        &format!("{prefix}base\n"),
        &format!("{prefix}ours\n"),
        &format!("{prefix}theirs\n"),
    );
    session.set_merge_advice_enabled(true);
    assert_eq!(session.unresolved_conflicts(), 1);
    session.go_to(Pane::Result, 0, 0);
    key(&mut session, KeyCode::Char('i'));
    key(&mut session, KeyCode::Char('X'));
    key(&mut session, KeyCode::Esc);
    assert!(session.is_local_diff_pending());
    assert!(matches!(
        session.begin_merge_advice(),
        Err(MergeAdviceError::Unavailable)
    ));
    assert_eq!(session.unresolved_conflicts(), 1);
    assert!(!session.is_confirming_merge());
}

#[test]
fn request_captures_selected_source_regions_not_the_whole_file() {
    let mut session = merge();
    key(&mut session, KeyCode::Char(']'));
    let request = session.begin_merge_advice().unwrap();
    let input = request.input();
    assert_eq!(input.base.lines, 4..5);
    assert_eq!(input.ours.lines, 4..5);
    assert_eq!(input.theirs.lines, 4..5);
    assert_eq!(input.base.snapshot.text(), BASE);
    assert_eq!(input.ours.snapshot.text(), OURS);
    assert_eq!(input.theirs.snapshot.text(), THEIRS);
    assert_eq!(session.pane_text(Pane::Result), OURS);
    assert_eq!(session.unresolved_conflicts(), 2);
    assert!(!session.is_dirty());
}

#[test]
fn duplicate_requests_do_not_replace_the_pending_request() {
    let mut session = merge();
    let request = session.begin_merge_advice().unwrap();
    assert!(matches!(
        session.begin_merge_advice(),
        Err(MergeAdviceError::Busy)
    ));
    assert!(session.receive_merge_advice(request, Ok(suggestion(MergeAdviceChoice::Theirs))));
    key(&mut session, KeyCode::Enter);
    assert_eq!(
        session.pane_text(Pane::Result),
        "head\ntheirs-one\nkeep\nstay\nours-two\ntail\n"
    );
    assert_eq!(session.unresolved_conflicts(), 1);
}

#[test]
fn receiving_and_dismissing_advice_never_mutates_or_requests_a_write() {
    for dismiss in [KeyCode::Esc, KeyCode::Char('q')] {
        for confidence in [0.0, 1.0] {
            let mut session = merge();
            let request = session.begin_merge_advice().unwrap();
            let mut reply = suggestion(MergeAdviceChoice::Theirs);
            reply.confidence = confidence;
            assert!(session.receive_merge_advice(request, Ok(reply)));
            assert_eq!(session.pane_text(Pane::Result), OURS);
            assert_eq!(session.unresolved_conflicts(), 2);
            assert!(!session.is_dirty());
            assert!(!session.is_confirming_merge());
            assert!(session.is_review_modal());
            assert_eq!(key(&mut session, dismiss), ReviewOutcome::Continue);
            assert_eq!(session.pane_text(Pane::Result), OURS);
            assert_eq!(session.unresolved_conflicts(), 2);
            assert!(!session.is_dirty());
            assert!(!session.is_confirming_merge());
            assert!(!session.is_review_modal());
            assert_eq!(key(&mut session, KeyCode::Char('q')), ReviewOutcome::Quit);
        }
    }
}

#[test]
fn explicit_apply_changes_only_the_requested_conflict_and_is_undoable() {
    for (second, expected) in [
        (false, "head\ntheirs-one\nkeep\nstay\nours-two\ntail\n"),
        (true, "head\nours-one\nkeep\nstay\ntheirs-two\ntail\n"),
    ] {
        let mut session = merge();
        if second {
            key(&mut session, KeyCode::Char(']'));
        }
        let request = session.begin_merge_advice().unwrap();
        assert!(session.receive_merge_advice(request, Ok(suggestion(MergeAdviceChoice::Theirs))));
        assert_eq!(key(&mut session, KeyCode::Enter), ReviewOutcome::Continue);
        assert_eq!(session.pane_text(Pane::Result), expected);
        assert_eq!(session.pane_text(Pane::Ours), OURS);
        assert_eq!(session.pane_text(Pane::Theirs), THEIRS);
        assert_eq!(session.unresolved_conflicts(), 1);
        assert!(session.is_dirty());
        assert!(!session.is_confirming_merge());
        assert_eq!(
            key(&mut session, KeyCode::Char('s')),
            ReviewOutcome::UnresolvedConflicts(1)
        );
        key(&mut session, KeyCode::Char('u'));
        assert_eq!(session.pane_text(Pane::Result), OURS);
        assert_eq!(session.unresolved_conflicts(), 2);
        ctrl_r(&mut session);
        assert_eq!(session.pane_text(Pane::Result), expected);
        assert_eq!(session.unresolved_conflicts(), 1);
    }
}

#[test]
fn applying_the_last_suggestion_requires_separate_write_confirmation() {
    for (choice, expected) in [
        (MergeAdviceChoice::Ours, "ours\n"),
        (MergeAdviceChoice::Theirs, "theirs\n"),
    ] {
        let mut session = ReviewSession::three_way("base\n", "ours\n", "theirs\n");
        session.set_merge_advice_enabled(true);
        let request = session.begin_merge_advice().unwrap();
        assert!(session.receive_merge_advice(request, Ok(suggestion(choice))));
        assert!(!session.is_confirming_merge());
        assert_eq!(key(&mut session, KeyCode::Enter), ReviewOutcome::Continue);
        assert_eq!(session.pane_text(Pane::Result), expected);
        assert_eq!(session.unresolved_conflicts(), 0);
        assert!(session.is_confirming_merge());
        assert_eq!(key(&mut session, KeyCode::Enter), ReviewOutcome::Continue);
        assert_eq!(key(&mut session, KeyCode::Esc), ReviewOutcome::Continue);
        assert!(!session.is_confirming_merge());
        assert_eq!(
            key(&mut session, KeyCode::Char('y')),
            ReviewOutcome::Continue
        );
        assert_eq!(
            key(&mut session, KeyCode::Char('s')),
            ReviewOutcome::Continue
        );
        assert!(session.is_confirming_merge());
        assert!(matches!(
            key(&mut session, KeyCode::Char('y')),
            ReviewOutcome::Submitted(submission) if submission.result.as_deref() == Some(expected)
        ));
    }
}

#[test]
fn byte_identical_ours_advice_has_a_metadata_undo_and_redo_step() {
    for (ours, theirs) in [("ours\n", "theirs\n"), ("", "theirs\n")] {
        let mut session = ReviewSession::three_way("base\n", ours, theirs);
        session.set_merge_advice_enabled(true);
        let request = session.begin_merge_advice().unwrap();
        assert!(session.receive_merge_advice(request, Ok(suggestion(MergeAdviceChoice::Ours))));
        assert_eq!(key(&mut session, KeyCode::Enter), ReviewOutcome::Continue);
        assert_eq!(session.pane_text(Pane::Result), ours);
        assert_eq!(session.unresolved_conflicts(), 0);
        assert!(session.is_confirming_merge());
        key(&mut session, KeyCode::Esc);
        key(&mut session, KeyCode::Char('u'));
        assert_eq!(session.pane_text(Pane::Result), ours);
        assert_eq!(session.unresolved_conflicts(), 1);
        assert!(!session.is_confirming_merge());
        assert_eq!(
            key(&mut session, KeyCode::Char('s')),
            ReviewOutcome::UnresolvedConflicts(1)
        );
        assert_eq!(ctrl_r(&mut session), ReviewOutcome::Continue);
        assert_eq!(session.pane_text(Pane::Result), ours);
        assert_eq!(session.unresolved_conflicts(), 0);
        assert!(session.is_confirming_merge());
        assert!(matches!(
            key(&mut session, KeyCode::Char('y')),
            ReviewOutcome::Submitted(submission) if submission.result.as_deref() == Some(ours)
        ));
    }
}

#[test]
fn leave_unresolved_has_no_apply_action_and_keeps_manual_resolution_available() {
    let mut session = merge();
    let request = session.begin_merge_advice().unwrap();
    assert!(
        session.receive_merge_advice(request, Ok(suggestion(MergeAdviceChoice::LeaveUnresolved)))
    );
    assert_eq!(key(&mut session, KeyCode::Enter), ReviewOutcome::Continue);
    assert_eq!(session.pane_text(Pane::Result), OURS);
    assert_eq!(session.unresolved_conflicts(), 2);
    assert!(!session.is_dirty());
    assert!(!session.is_confirming_merge());
    key(&mut session, KeyCode::Esc);
    key(&mut session, KeyCode::Char('t'));
    assert_eq!(session.unresolved_conflicts(), 1);
    assert_eq!(
        session.pane_text(Pane::Result),
        "head\ntheirs-one\nkeep\nstay\nours-two\ntail\n"
    );
}

#[test]
fn selection_away_and_back_does_not_revive_pending_advice() {
    let mut session = merge();
    let request = session.begin_merge_advice().unwrap();
    key(&mut session, KeyCode::Char(']'));
    assert_eq!(session.selected_hunk(), Some(1));
    key(&mut session, KeyCode::Char('['));
    assert_eq!(session.selected_hunk(), Some(0));
    assert!(!session.receive_merge_advice(request, Ok(suggestion(MergeAdviceChoice::Theirs))));
    key(&mut session, KeyCode::Enter);
    assert_eq!(session.pane_text(Pane::Result), OURS);
    assert_eq!(session.unresolved_conflicts(), 2);
    assert!(!session.is_dirty());
}

#[test]
fn editing_while_a_request_is_pending_preserves_manual_work_against_a_late_reply() {
    let mut session = merge();
    let request = session.begin_merge_advice().unwrap();
    session.go_to(Pane::Result, 1, 0);
    key(&mut session, KeyCode::Char('i'));
    key(&mut session, KeyCode::Char('X'));
    key(&mut session, KeyCode::Esc);
    assert!(!session.receive_merge_advice(request, Ok(suggestion(MergeAdviceChoice::Theirs))));
    key(&mut session, KeyCode::Enter);
    assert_eq!(
        session.pane_text(Pane::Result),
        "head\nXours-one\nkeep\nstay\nours-two\ntail\n"
    );
    assert_eq!(session.unresolved_conflicts(), 2);
    key(&mut session, KeyCode::Char('r'));
    assert_eq!(session.unresolved_conflicts(), 1);
}

#[test]
fn edit_and_undo_to_identical_bytes_does_not_revive_pending_advice() {
    let mut session = merge();
    let request = session.begin_merge_advice().unwrap();
    session.go_to(Pane::Result, 1, 0);
    key(&mut session, KeyCode::Char('i'));
    key(&mut session, KeyCode::Char('X'));
    key(&mut session, KeyCode::Esc);
    key(&mut session, KeyCode::Char('u'));
    assert_eq!(session.pane_text(Pane::Result), OURS);
    assert_eq!(session.unresolved_conflicts(), 2);
    assert_eq!(session.selected_hunk(), Some(0));
    assert!(!session.receive_merge_advice(request, Ok(suggestion(MergeAdviceChoice::Theirs))));
    key(&mut session, KeyCode::Enter);
    assert_eq!(session.pane_text(Pane::Result), OURS);
    assert_eq!(session.unresolved_conflicts(), 2);
}

#[test]
fn resolution_and_undo_with_unchanged_bytes_does_not_revive_pending_advice() {
    let mut session = merge();
    let request = session.begin_merge_advice().unwrap();
    key(&mut session, KeyCode::Char('o'));
    assert_eq!(session.pane_text(Pane::Result), OURS);
    assert_eq!(session.unresolved_conflicts(), 1);
    key(&mut session, KeyCode::Char('u'));
    assert_eq!(session.pane_text(Pane::Result), OURS);
    assert_eq!(session.unresolved_conflicts(), 2);
    assert_eq!(session.selected_hunk(), Some(0));
    assert!(!session.receive_merge_advice(request, Ok(suggestion(MergeAdviceChoice::Theirs))));
    key(&mut session, KeyCode::Enter);
    assert_eq!(session.pane_text(Pane::Result), OURS);
    assert_eq!(session.unresolved_conflicts(), 2);
}

#[test]
fn help_round_trip_invalidates_pending_advice_without_changing_the_merge() {
    let mut session = merge();
    let request = session.begin_merge_advice().unwrap();
    key(&mut session, KeyCode::Char('?'));
    key(&mut session, KeyCode::Esc);
    assert!(!session.receive_merge_advice(request, Ok(suggestion(MergeAdviceChoice::Theirs))));
    key(&mut session, KeyCode::Enter);
    assert_eq!(session.pane_text(Pane::Result), OURS);
    assert_eq!(session.unresolved_conflicts(), 2);
    assert!(!session.is_dirty());
}

#[test]
fn a_reply_for_an_identical_other_session_cannot_consume_the_current_request() {
    let mut first = merge();
    let mut second = merge();
    let wrong = first.begin_merge_advice().unwrap();
    let current = second.begin_merge_advice().unwrap();
    assert!(!second.receive_merge_advice(wrong, Ok(suggestion(MergeAdviceChoice::Ours))));
    key(&mut second, KeyCode::Enter);
    assert_eq!(second.unresolved_conflicts(), 2);
    assert!(!second.is_dirty());
    assert!(second.receive_merge_advice(current, Ok(suggestion(MergeAdviceChoice::Theirs))));
    key(&mut second, KeyCode::Enter);
    assert_eq!(
        second.pane_text(Pane::Result),
        "head\ntheirs-one\nkeep\nstay\nours-two\ntail\n"
    );
    assert_eq!(second.unresolved_conflicts(), 1);
    assert_eq!(first.pane_text(Pane::Result), OURS);
    assert_eq!(first.unresolved_conflicts(), 2);
}

#[test]
fn cancellation_discards_late_success_and_error_replies() {
    for reply in [
        Ok(suggestion(MergeAdviceChoice::Theirs)),
        Err("Advice service unavailable".to_owned()),
    ] {
        let mut session = merge();
        let request = session.begin_merge_advice().unwrap();
        session.cancel_merge_advice();
        assert!(!session.receive_merge_advice(request, reply));
        key(&mut session, KeyCode::Enter);
        assert_eq!(session.pane_text(Pane::Result), OURS);
        assert_eq!(session.unresolved_conflicts(), 2);
        assert!(!session.is_dirty());
        let next = session.begin_merge_advice().unwrap();
        assert!(session.receive_merge_advice(next, Ok(suggestion(MergeAdviceChoice::Theirs))));
        key(&mut session, KeyCode::Enter);
        assert_eq!(session.unresolved_conflicts(), 1);
    }
}

#[test]
fn cancelling_displayed_advice_revokes_its_apply_authority() {
    let mut session = merge();
    let request = session.begin_merge_advice().unwrap();
    assert!(session.receive_merge_advice(request, Ok(suggestion(MergeAdviceChoice::Theirs))));
    session.cancel_merge_advice();
    key(&mut session, KeyCode::Enter);
    assert_eq!(session.pane_text(Pane::Result), OURS);
    assert_eq!(session.unresolved_conflicts(), 2);
    assert!(!session.is_dirty());
    key(&mut session, KeyCode::Char('t'));
    assert_eq!(session.unresolved_conflicts(), 1);
}

#[test]
fn disabling_advice_revokes_pending_and_displayed_authority() {
    for display in [false, true] {
        let mut session = merge();
        let request = session.begin_merge_advice().unwrap();
        if display {
            assert!(
                session.receive_merge_advice(
                    request.clone(),
                    Ok(suggestion(MergeAdviceChoice::Theirs))
                )
            );
        }
        session.set_merge_advice_enabled(false);
        assert!(!session.receive_merge_advice(request, Ok(suggestion(MergeAdviceChoice::Theirs))));
        key(&mut session, KeyCode::Enter);
        assert_eq!(session.pane_text(Pane::Result), OURS);
        assert_eq!(session.unresolved_conflicts(), 2);
        assert!(!session.is_dirty());
    }
}

#[test]
fn quitting_during_a_request_rejects_a_late_response() {
    let mut session = merge();
    let request = session.begin_merge_advice().unwrap();
    assert_eq!(key(&mut session, KeyCode::Char('q')), ReviewOutcome::Quit);
    assert!(!session.receive_merge_advice(request, Ok(suggestion(MergeAdviceChoice::Theirs))));
    assert_eq!(session.pane_text(Pane::Result), OURS);
    assert_eq!(session.unresolved_conflicts(), 2);
    assert!(!session.is_dirty());
}

#[test]
fn current_error_clears_pending_state_and_preserves_retry_and_manual_resolution() {
    let mut session = merge();
    let request = session.begin_merge_advice().unwrap();
    assert!(session.receive_merge_advice(request, Err("Advice service unavailable".to_owned())));
    assert_eq!(session.pane_text(Pane::Result), OURS);
    assert_eq!(session.unresolved_conflicts(), 2);
    assert!(!session.is_dirty());
    assert!(!session.is_confirming_merge());
    let retry = session.begin_merge_advice().unwrap();
    assert!(session.receive_merge_advice(retry, Ok(suggestion(MergeAdviceChoice::Theirs))));
    key(&mut session, KeyCode::Esc);
    key(&mut session, KeyCode::Char('t'));
    assert_eq!(
        session.pane_text(Pane::Result),
        "head\ntheirs-one\nkeep\nstay\nours-two\ntail\n"
    );
    assert_eq!(session.unresolved_conflicts(), 1);
}

#[test]
fn a_consumed_request_cannot_reopen_a_dismissed_suggestion() {
    let mut session = merge();
    let request = session.begin_merge_advice().unwrap();
    assert!(
        session.receive_merge_advice(request.clone(), Ok(suggestion(MergeAdviceChoice::Theirs)))
    );
    key(&mut session, KeyCode::Esc);
    assert!(!session.receive_merge_advice(request, Ok(suggestion(MergeAdviceChoice::Theirs))));
    key(&mut session, KeyCode::Enter);
    assert_eq!(session.pane_text(Pane::Result), OURS);
    assert_eq!(session.unresolved_conflicts(), 2);
    assert!(!session.is_dirty());
}

#[test]
fn suggestion_modal_blocks_underlying_keys_paste_and_mouse_actions() {
    let mut session = merge();
    key(&mut session, KeyCode::Char('t'));
    session.handle(ReviewInput::Resize {
        width: 120,
        height: 20,
    });
    let before = draw(&session, 120, 20);
    let hit = before
        .content
        .iter()
        .enumerate()
        .find(|(index, cell)| {
            let row = index / 120;
            row >= 2 && row < 19 && cell.symbol() == "«"
        })
        .map(|(index, _)| index)
        .unwrap();
    let request = session.begin_merge_advice().unwrap();
    assert!(session.receive_merge_advice(request, Ok(suggestion(MergeAdviceChoice::Theirs))));
    let cursor = session.cursor();
    let selected = session.selected_hunk();
    for code in [
        KeyCode::Char(']'),
        KeyCode::Char('['),
        KeyCode::Down,
        KeyCode::Up,
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::PageDown,
        KeyCode::PageUp,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::Tab,
        KeyCode::BackTab,
        KeyCode::Char('o'),
        KeyCode::Char('t'),
        KeyCode::Char('b'),
        KeyCode::Char('r'),
        KeyCode::Char('a'),
        KeyCode::Char('i'),
        KeyCode::Char('u'),
        KeyCode::Char('s'),
        KeyCode::Char('y'),
        KeyCode::Char('?'),
    ] {
        assert_eq!(key(&mut session, code), ReviewOutcome::Continue);
        assert_eq!(session.cursor(), cursor);
        assert_eq!(session.selected_hunk(), selected);
        assert_eq!(session.unresolved_conflicts(), 1);
        assert_eq!(
            session.pane_text(Pane::Result),
            "head\ntheirs-one\nkeep\nstay\nours-two\ntail\n"
        );
        assert!(!session.is_editing());
        assert!(!session.is_confirming_merge());
    }
    assert_eq!(
        session.handle(ReviewInput::Paste("accidental edit".to_owned())),
        ReviewOutcome::Continue
    );
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::ScrollDown,
    ] {
        assert_eq!(
            session.handle(ReviewInput::Mouse(MouseEvent {
                kind,
                column: (hit % 120) as u16,
                row: (hit / 120) as u16,
                modifiers: KeyModifiers::NONE,
            })),
            ReviewOutcome::Continue
        );
        assert_eq!(session.cursor(), cursor);
        assert_eq!(session.selected_hunk(), selected);
        assert_eq!(session.unresolved_conflicts(), 1);
    }
    assert_eq!(key(&mut session, KeyCode::Enter), ReviewOutcome::Continue);
    assert_eq!(session.pane_text(Pane::Result), THEIRS);
    assert_eq!(session.unresolved_conflicts(), 0);
    assert!(session.is_confirming_merge());
}

#[test]
fn suggestion_modal_blocks_redo_of_an_undone_resolution() {
    let mut session = merge();
    key(&mut session, KeyCode::Char('t'));
    key(&mut session, KeyCode::Char('u'));
    let request = session.begin_merge_advice().unwrap();
    assert!(session.receive_merge_advice(request, Ok(suggestion(MergeAdviceChoice::Ours))));
    assert_eq!(ctrl_r(&mut session), ReviewOutcome::Continue);
    assert_eq!(session.pane_text(Pane::Result), OURS);
    assert_eq!(session.unresolved_conflicts(), 2);
    key(&mut session, KeyCode::Enter);
    assert_eq!(session.pane_text(Pane::Result), OURS);
    assert_eq!(session.unresolved_conflicts(), 1);
}

#[test]
fn reviewing_a_suggestion_does_not_cover_the_merge() {
    let mut session = merge();
    session.handle(ReviewInput::Resize {
        width: 120,
        height: 20,
    });
    let before = draw(&session, 120, 20);
    let request = session.begin_merge_advice().unwrap();
    assert!(session.receive_merge_advice(request, Ok(suggestion(MergeAdviceChoice::Theirs))));
    let after = draw(&session, 120, 20);
    for y in 0..19 {
        for x in 0..120 {
            assert_eq!(before[(x, y)], after[(x, y)], "merge obscured at {x},{y}");
        }
    }
}

#[test]
fn abstaining_does_not_resolve_or_dirty_the_conflict() {
    let mut session = merge();
    let request = session.begin_merge_advice().unwrap();
    assert!(
        session.receive_merge_advice(request, Ok(suggestion(MergeAdviceChoice::LeaveUnresolved)))
    );
    assert_eq!(key(&mut session, KeyCode::Enter), ReviewOutcome::Continue);
    assert_eq!(session.pane_text(Pane::Result), OURS);
    assert_eq!(session.unresolved_conflicts(), 2);
    assert!(!session.is_dirty());
}
