use chvrn_core::TextSnapshot;
use chvrn_tui::{
    Pane, ReviewEditError, ReviewInput, ReviewOutcome, ReviewSession, WhitespacePolicy,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};
use std::time::{Duration, Instant};

fn key(session: &mut ReviewSession, code: KeyCode) -> ReviewOutcome {
    session.handle(ReviewInput::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

fn ctrl_r(session: &mut ReviewSession) -> ReviewOutcome {
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Char('r'),
        KeyModifiers::CONTROL,
    )))
}

fn click_gutter_control(session: &mut ReviewSession, symbol: &str) -> ReviewOutcome {
    session.handle(ReviewInput::Resize {
        width: 120,
        height: 20,
    });
    let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
    terminal.draw(|frame| session.render(frame)).unwrap();
    let buffer = terminal.backend().buffer();
    let width = usize::from(buffer.area.width);
    let hit = buffer
        .content
        .iter()
        .enumerate()
        .find(|(index, cell)| {
            let row = index / width;
            row >= 2 && row + 1 < usize::from(buffer.area.height) && cell.symbol() == symbol
        })
        .map(|(index, _)| index)
        .unwrap_or_else(|| panic!("missing gutter control {symbol}"));
    session.handle(ReviewInput::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: (hit % width) as u16,
        row: (hit / width) as u16,
        modifiers: KeyModifiers::NONE,
    }))
}

#[test]
fn gutter_insertion_places_the_opposite_block_above_or_below_the_chosen_block() {
    for (choice, symbol, expected) in [
        ('o', "↖", "head\ntheirs\nours-a\nours-b\ntail\n"),
        ('o', "↙", "head\nours-a\nours-b\ntheirs\ntail\n"),
        ('t', "↗", "head\nours-a\nours-b\ntheirs\ntail\n"),
        ('t', "↘", "head\ntheirs\nours-a\nours-b\ntail\n"),
    ] {
        let mut session = ReviewSession::three_way(
            "head\nbase\ntail\n",
            "head\nours-a\nours-b\ntail\n",
            "head\ntheirs\ntail\n",
        );
        key(&mut session, KeyCode::Char(choice));
        assert_eq!(session.unresolved_conflicts(), 0);

        assert!(matches!(
            click_gutter_control(&mut session, symbol),
            ReviewOutcome::Continue
        ));

        assert_eq!(session.pane_text(Pane::Result), expected);
        assert_eq!(session.unresolved_conflicts(), 0);
        assert_eq!(
            session.pane_text(Pane::Ours),
            "head\nours-a\nours-b\ntail\n"
        );
        assert_eq!(session.pane_text(Pane::Theirs), "head\ntheirs\ntail\n");
        assert!(matches!(
            key(&mut session, KeyCode::Char('s')),
            ReviewOutcome::Submitted(submission) if submission.result.as_deref() == Some(expected)
        ));
    }
}

#[test]
fn gutter_insertion_preserves_duplicate_lines_in_the_two_blocks() {
    let mut session = ReviewSession::three_way(
        "head\nbase-a\nbase-middle\nbase-b\ntail\n",
        "head\nours-a\nshared\nours-b\ntail\n",
        "head\ntheirs-a\nshared\ntheirs-b\ntail\n",
    );
    assert_eq!(session.unresolved_conflicts(), 1);
    key(&mut session, KeyCode::Char('o'));

    click_gutter_control(&mut session, "↙");

    assert_eq!(
        session.pane_text(Pane::Result),
        "head\nours-a\nshared\nours-b\ntheirs-a\nshared\ntheirs-b\ntail\n"
    );
}

#[test]
fn gutter_insertion_undo_restores_the_choice_before_undoing_the_resolution() {
    let mut session = ReviewSession::three_way(
        "head\nbase\ntail\n",
        "head\nours\ntail\n",
        "head\ntheirs\ntail\n",
    );
    key(&mut session, KeyCode::Char('o'));
    click_gutter_control(&mut session, "↙");

    key(&mut session, KeyCode::Char('u'));
    assert_eq!(session.pane_text(Pane::Result), "head\nours\ntail\n");
    assert_eq!(session.unresolved_conflicts(), 0);

    ctrl_r(&mut session);
    assert_eq!(
        session.pane_text(Pane::Result),
        "head\nours\ntheirs\ntail\n"
    );
    assert_eq!(session.unresolved_conflicts(), 0);

    key(&mut session, KeyCode::Char('u'));
    key(&mut session, KeyCode::Char('u'));
    assert_eq!(session.pane_text(Pane::Result), "head\nours\ntail\n");
    assert_eq!(session.unresolved_conflicts(), 1);

    click_gutter_control(&mut session, "«");
    assert_eq!(session.pane_text(Pane::Result), "head\ntheirs\ntail\n");
    assert_eq!(session.unresolved_conflicts(), 0);
}

#[test]
fn gutter_insertion_leaves_later_conflicts_unresolved_and_targetable() {
    let mut session = ReviewSession::three_way(
        "head\nbase-one\nkeep\nstay\nbase-two\ntail\n",
        "head\nλours-one-a\nours-one-b\nkeep\nstay\nours-two\ntail\n",
        "head\ntheirs-one\nkeep\nstay\ntheirs-two-a\ntheirs-two-b\ntail\n",
    );
    assert_eq!(session.unresolved_conflicts(), 2);
    key(&mut session, KeyCode::Char('t'));

    click_gutter_control(&mut session, "↗");

    assert_eq!(
        session.pane_text(Pane::Result),
        "head\nλours-one-a\nours-one-b\ntheirs-one\nkeep\nstay\nours-two\ntail\n"
    );
    assert_eq!(session.unresolved_conflicts(), 1);
    assert!(matches!(
        key(&mut session, KeyCode::Char('s')),
        ReviewOutcome::UnresolvedConflicts(1)
    ));

    assert!(matches!(
        click_gutter_control(&mut session, "«"),
        ReviewOutcome::Continue
    ));

    assert_eq!(
        session.pane_text(Pane::Result),
        "head\nλours-one-a\nours-one-b\ntheirs-one\nkeep\nstay\ntheirs-two-a\ntheirs-two-b\ntail\n"
    );
    assert_eq!(session.unresolved_conflicts(), 0);
    assert!(matches!(
        key(&mut session, KeyCode::Char('s')),
        ReviewOutcome::Submitted(submission)
            if submission.result.as_deref() == Some(
                "head\nλours-one-a\nours-one-b\ntheirs-one\nkeep\nstay\ntheirs-two-a\ntheirs-two-b\ntail\n"
            )
    ));
}

#[test]
fn gutter_insertion_preserves_manual_edits_to_the_result() {
    let mut session = ReviewSession::three_way(
        "head\nbase\ntail\n",
        "head\nours\ntail\n",
        "head\ntheirs\ntail\n",
    );
    key(&mut session, KeyCode::Char('o'));
    session.go_to(Pane::Result, 0, 0);
    key(&mut session, KeyCode::Char('i'));
    key(&mut session, KeyCode::Char('X'));
    key(&mut session, KeyCode::Esc);
    session.go_to(Pane::Result, 1, 0);
    key(&mut session, KeyCode::Char('i'));
    key(&mut session, KeyCode::Char('Y'));
    key(&mut session, KeyCode::Esc);

    click_gutter_control(&mut session, "↙");

    assert_eq!(
        session.pane_text(Pane::Result),
        "Xhead\nYours\ntheirs\ntail\n"
    );
    assert_eq!(session.pane_text(Pane::Ours), "head\nours\ntail\n");
    assert_eq!(session.pane_text(Pane::Theirs), "head\ntheirs\ntail\n");
    assert_eq!(session.unresolved_conflicts(), 0);
}

#[test]
fn gutter_insertion_can_restore_code_after_choosing_a_deletion() {
    for (ours, theirs, choice, symbol) in [
        ("head\ntail\n", "head\ntheirs\ntail\n", 'o', "↙"),
        ("head\ntheirs\ntail\n", "head\ntail\n", 't', "↗"),
    ] {
        let mut session = ReviewSession::three_way("head\nbase\ntail\n", ours, theirs);
        key(&mut session, KeyCode::Char(choice));
        assert_eq!(session.pane_text(Pane::Result), "head\ntail\n");
        assert_eq!(session.unresolved_conflicts(), 0);

        click_gutter_control(&mut session, symbol);

        assert_eq!(session.pane_text(Pane::Result), "head\ntheirs\ntail\n");
        assert_eq!(session.unresolved_conflicts(), 0);
    }
}

#[test]
fn gutter_chevrons_apply_only_the_selected_difference() {
    for (symbol, destination, source, expected, unchanged) in [
        (
            "»",
            Pane::Right,
            Pane::Left,
            "head\nours-one\nkeep\nstay\ntheirs-two\ntail\n",
            "head\nours-one\nkeep\nstay\nours-two\ntail\n",
        ),
        (
            "«",
            Pane::Left,
            Pane::Right,
            "head\ntheirs-one\nkeep\nstay\nours-two\ntail\n",
            "head\ntheirs-one\nkeep\nstay\ntheirs-two\ntail\n",
        ),
    ] {
        let mut session = ReviewSession::two_way(
            "head\nours-one\nkeep\nstay\nours-two\ntail\n",
            "head\ntheirs-one\nkeep\nstay\ntheirs-two\ntail\n",
        );

        click_gutter_control(&mut session, symbol);

        assert_eq!(session.pane_text(destination), expected);
        assert_eq!(session.pane_text(source), unchanged);
        assert_eq!(session.hunk_count(), 1);
    }
}

#[test]
fn navigating_an_insertion_skips_virtual_rows_without_skipping_source_lines() {
    let mut session = ReviewSession::two_way("top\nend\n", "top\none\ntwo\nthree\nend\n");

    key(&mut session, KeyCode::Char('j'));
    assert_eq!(session.cursor().pane, Pane::Left);
    assert_eq!(session.cursor().line, 1);
    key(&mut session, KeyCode::Char('k'));
    assert_eq!(session.cursor().line, 0);
    key(&mut session, KeyCode::Char('j'));
    key(&mut session, KeyCode::Char('i'));
    key(&mut session, KeyCode::Char('X'));

    assert_eq!(session.pane_text(Pane::Left), "top\nXend\n");
}

#[test]
fn clicking_below_an_insertion_edits_the_displayed_source_line() {
    let mut session = ReviewSession::two_way("top\nend\n", "top\none\ntwo\nthree\nend\n");
    session.handle(ReviewInput::Resize {
        width: 80,
        height: 9,
    });
    let mut terminal = Terminal::new(TestBackend::new(80, 9)).unwrap();
    terminal.draw(|frame| session.render(frame)).unwrap();
    let buffer = terminal.backend().buffer();
    let (column, row) = (2..8)
        .flat_map(|y| (0..37).map(move |x| (x, y)))
        .find(|&(x, y)| {
            buffer[(x, y)].symbol() == "e"
                && buffer[(x + 1, y)].symbol() == "n"
                && buffer[(x + 2, y)].symbol() == "d"
        })
        .expect("source end line is visible");
    session.handle(ReviewInput::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }));
    assert_eq!(session.cursor().line, 1);
    key(&mut session, KeyCode::Char('i'));
    key(&mut session, KeyCode::Char('X'));
    assert_eq!(session.pane_text(Pane::Left), "top\nXend\n");
}

#[test]
fn clicking_a_theirs_control_resolves_only_its_conflict_and_undo_restores_it() {
    let mut session = ReviewSession::three_way(
        "head\nbase-one\nkeep\nstay\nbase-two\ntail\n",
        "head\nours-one\nkeep\nstay\nours-two\ntail\n",
        "head\ntheirs-one\nkeep\nstay\ntheirs-two\ntail\n",
    );
    click_gutter_control(&mut session, "«");

    assert_eq!(session.unresolved_conflicts(), 1);
    assert_eq!(
        session.pane_text(Pane::Result),
        "head\ntheirs-one\nkeep\nstay\nours-two\ntail\n"
    );
    assert_eq!(
        session.pane_text(Pane::Ours),
        "head\nours-one\nkeep\nstay\nours-two\ntail\n"
    );
    key(&mut session, KeyCode::Char('u'));
    assert_eq!(session.unresolved_conflicts(), 2);
    assert_eq!(
        session.pane_text(Pane::Result),
        "head\nours-one\nkeep\nstay\nours-two\ntail\n"
    );
}

#[test]
fn hunk_navigation_moves_between_distinct_changes_and_stops_at_the_ends() {
    let mut session = ReviewSession::two_way(
        "start\nold-one\nkeep-a\nkeep-b\nold-two\nend\n",
        "start\nnew-one\nkeep-a\nkeep-b\nnew-two\nend\n",
    );

    assert_eq!(session.hunk_count(), 2);
    assert_eq!(session.selected_hunk(), Some(0));
    key(&mut session, KeyCode::Char(']'));
    assert_eq!(session.selected_hunk(), Some(1));
    assert_eq!(session.selected_hunk_ranges(), Some((4..5, 4..5)));
    assert_eq!(session.cursor().aligned_row, 4);
    key(&mut session, KeyCode::Char(']'));
    assert_eq!(session.selected_hunk(), Some(1));
    key(&mut session, KeyCode::Char('['));
    assert_eq!(session.selected_hunk(), Some(0));
    assert_eq!(session.selected_hunk_ranges(), Some((1..2, 1..2)));
    assert_eq!(session.cursor().aligned_row, 1);
}

#[test]
fn applying_from_left_replaces_only_the_selected_right_hunk() {
    let mut session = ReviewSession::two_way(
        "header\nleft\nsteady-a\nsteady-b\nleft-tail\n",
        "header\nright\nsteady-a\nsteady-b\nright-tail\n",
    );

    assert_eq!(session.selected_hunk(), Some(0));
    key(&mut session, KeyCode::Char('a'));

    assert_eq!(
        session.pane_text(Pane::Right),
        "header\nleft\nsteady-a\nsteady-b\nright-tail\n"
    );
    assert_eq!(
        session.pane_text(Pane::Left),
        "header\nleft\nsteady-a\nsteady-b\nleft-tail\n"
    );
}

#[test]
fn applying_from_right_replaces_only_the_selected_left_hunk() {
    let mut session = ReviewSession::two_way(
        "header\nleft\nsteady-a\nsteady-b\nleft-tail\n",
        "header\nright\nsteady-a\nsteady-b\nright-tail\n",
    );

    key(&mut session, KeyCode::Tab);
    assert_eq!(session.focus(), Pane::Right);
    key(&mut session, KeyCode::Char('a'));

    assert_eq!(
        session.pane_text(Pane::Left),
        "header\nright\nsteady-a\nsteady-b\nleft-tail\n"
    );
    assert_eq!(
        session.pane_text(Pane::Right),
        "header\nright\nsteady-a\nsteady-b\nright-tail\n"
    );
}

#[test]
fn undo_after_applying_a_hunk_restores_the_destination_not_the_focused_source() {
    let mut session = ReviewSession::two_way("keep\nleft\n", "keep\nright\n");
    key(&mut session, KeyCode::Char('a'));
    assert_eq!(session.pane_text(Pane::Right), "keep\nleft\n");

    key(&mut session, KeyCode::Char('u'));
    assert_eq!(session.pane_text(Pane::Right), "keep\nright\n");
    assert_eq!(session.pane_text(Pane::Left), "keep\nleft\n");
    ctrl_r(&mut session);
    assert_eq!(session.pane_text(Pane::Right), "keep\nleft\n");
}

#[test]
fn applying_a_file_end_insertion_copies_the_exact_source_lines() {
    let mut session = ReviewSession::two_way("keep\n", "keep\nadded\n");
    assert_eq!(session.selected_hunk_ranges(), Some((1..1, 1..2)));
    key(&mut session, KeyCode::Char('a'));
    assert_eq!(session.pane_text(Pane::Right), "keep\n");

    let mut reverse = ReviewSession::two_way("keep\n", "keep\nadded\n");
    key(&mut reverse, KeyCode::Tab);
    key(&mut reverse, KeyCode::Char('a'));
    assert_eq!(reverse.pane_text(Pane::Left), "keep\nadded\n");
}

#[test]
fn edit_undo_and_redo_change_the_actual_review_buffer() {
    let mut session = ReviewSession::two_way("one\ntwo\n", "one\ntwo\n");

    key(&mut session, KeyCode::Char('i'));
    key(&mut session, KeyCode::Char('界'));
    key(&mut session, KeyCode::Esc);
    assert_eq!(session.pane_text(Pane::Left), "界one\ntwo\n");
    assert_eq!(session.cursor().grapheme, 1);

    key(&mut session, KeyCode::Char('u'));
    assert_eq!(session.pane_text(Pane::Left), "one\ntwo\n");
    ctrl_r(&mut session);
    assert_eq!(session.pane_text(Pane::Left), "界one\ntwo\n");
    assert_eq!(session.pane_text(Pane::Right), "one\ntwo\n");
}

#[test]
fn backspace_removes_one_combining_grapheme_not_an_individual_codepoint() {
    let mut session = ReviewSession::two_way("end\n", "end\n");

    key(&mut session, KeyCode::Char('i'));
    key(&mut session, KeyCode::Char('e'));
    key(&mut session, KeyCode::Char('\u{301}'));
    assert_eq!(session.pane_text(Pane::Left), "e\u{301}end\n");
    assert_eq!(session.cursor().grapheme, 1);
    key(&mut session, KeyCode::Backspace);

    assert_eq!(session.pane_text(Pane::Left), "end\n");
    assert_eq!(session.cursor().grapheme, 0);
}

#[test]
fn conflict_choices_use_the_chosen_bytes_and_allow_submission_only_after_resolution() {
    for (choice, expected) in [
        ('o', "head\nours\ntail\n"),
        ('t', "head\ntheirs\ntail\n"),
        ('b', "head\nours\ntheirs\ntail\n"),
    ] {
        let mut session = ReviewSession::three_way(
            "head\nbase\ntail\n",
            "head\nours\ntail\n",
            "head\ntheirs\ntail\n",
        );

        assert_eq!(session.unresolved_conflicts(), 1);
        assert_eq!(
            key(&mut session, KeyCode::Char('s')),
            ReviewOutcome::UnresolvedConflicts(1)
        );
        assert_eq!(session.unresolved_conflicts(), 1);
        key(&mut session, KeyCode::Char(choice));
        assert_eq!(session.unresolved_conflicts(), 0);
        assert_eq!(session.pane_text(Pane::Result), expected);
        assert_eq!(session.pane_text(Pane::Ours), "head\nours\ntail\n");
        assert_eq!(session.pane_text(Pane::Theirs), "head\ntheirs\ntail\n");
        assert!(matches!(
            key(&mut session, KeyCode::Char('s')),
            ReviewOutcome::Submitted(submission) if submission.result.as_deref() == Some(expected)
        ));
    }
}

#[test]
fn manually_edited_result_is_a_distinct_conflict_choice() {
    let mut session = ReviewSession::three_way("base\n", "ours\n", "theirs\n");

    assert_eq!(session.focus(), Pane::Result);
    assert_eq!(session.unresolved_conflicts(), 1);
    key(&mut session, KeyCode::Char('i'));
    key(&mut session, KeyCode::Char('X'));
    key(&mut session, KeyCode::Esc);
    assert_eq!(session.pane_text(Pane::Result), "Xours\n");
    assert_eq!(session.unresolved_conflicts(), 1);
    key(&mut session, KeyCode::Char('r'));

    assert_eq!(session.unresolved_conflicts(), 0);
    assert!(matches!(
        key(&mut session, KeyCode::Char('s')),
        ReviewOutcome::Submitted(submission) if submission.result.as_deref() == Some("Xours\n")
    ));
}

#[test]
fn dirty_quit_needs_discard_confirmation_but_submit_is_explicit() {
    let mut session = ReviewSession::two_way("original\n", "original\n");
    key(&mut session, KeyCode::Char('i'));
    key(&mut session, KeyCode::Char('X'));
    key(&mut session, KeyCode::Esc);

    assert_eq!(
        key(&mut session, KeyCode::Char('q')),
        ReviewOutcome::DiscardRequired
    );
    assert_eq!(session.pane_text(Pane::Left), "Xoriginal\n");
    assert_eq!(key(&mut session, KeyCode::Esc), ReviewOutcome::Continue);
    assert!(matches!(
        key(&mut session, KeyCode::Char('s')),
        ReviewOutcome::Submitted(submission)
            if submission.left == "Xoriginal\n" && submission.right == "original\n"
    ));
}

#[test]
fn confirmed_discard_quits_without_submitting_the_edits() {
    let mut session = ReviewSession::two_way("original\n", "original\n");
    key(&mut session, KeyCode::Char('i'));
    key(&mut session, KeyCode::Char('X'));
    key(&mut session, KeyCode::Esc);

    assert_eq!(
        key(&mut session, KeyCode::Char('q')),
        ReviewOutcome::DiscardRequired
    );
    assert_eq!(
        session.handle(ReviewInput::ConfirmDiscard),
        ReviewOutcome::Quit
    );
}

#[test]
fn older_background_diff_completion_cannot_replace_a_newer_snapshot() {
    let mut session = ReviewSession::two_way("original\n", "original\n");
    let older = session.request_diff("original\n", "old candidate\n");
    let newer = session.request_diff("original\n", "latest candidate\n");

    assert_eq!(
        session.handle(ReviewInput::DiffReady(newer.compute())),
        ReviewOutcome::Continue
    );
    assert_eq!(session.pane_text(Pane::Right), "latest candidate\n");
    assert_eq!(
        session.handle(ReviewInput::DiffReady(older.compute())),
        ReviewOutcome::Continue
    );
    assert_eq!(session.pane_text(Pane::Right), "latest candidate\n");
    assert_eq!(session.hunk_count(), 1);
}

#[test]
fn external_refresh_arriving_after_a_local_edit_requires_explicit_discard() {
    let mut session = ReviewSession::two_way("original\n", "original\n");
    let pending = session.request_diff("original\n", "external\n");
    key(&mut session, KeyCode::Char('i'));
    key(&mut session, KeyCode::Char('X'));
    key(&mut session, KeyCode::Esc);

    assert_eq!(
        session.handle(ReviewInput::DiffReady(pending.compute())),
        ReviewOutcome::RefreshConflict
    );
    assert_eq!(session.pane_text(Pane::Left), "Xoriginal\n");
    assert_eq!(session.pane_text(Pane::Right), "original\n");
    assert_eq!(
        key(&mut session, KeyCode::Char('s')),
        ReviewOutcome::RefreshConflict
    );
    assert_eq!(
        session.handle(ReviewInput::DiscardAndReload),
        ReviewOutcome::Continue
    );
    assert_eq!(session.pane_text(Pane::Left), "original\n");
    assert_eq!(session.pane_text(Pane::Right), "external\n");
    assert!(matches!(
        key(&mut session, KeyCode::Char('s')),
        ReviewOutcome::Submitted(submission)
            if submission.left == "original\n" && submission.right == "external\n"
    ));
}

#[test]
fn bracketed_paste_is_one_undoable_insert() {
    let mut session = ReviewSession::two_way("end\n", "end\n");
    key(&mut session, KeyCode::Char('i'));
    session.handle(ReviewInput::Paste("α\nβ".to_owned()));
    key(&mut session, KeyCode::Esc);
    assert_eq!(session.pane_text(Pane::Left), "α\nβend\n");

    key(&mut session, KeyCode::Char('u'));
    assert_eq!(session.pane_text(Pane::Left), "end\n");
    ctrl_r(&mut session);
    assert_eq!(session.pane_text(Pane::Left), "α\nβend\n");
}

#[test]
fn go_to_clamps_grapheme_column_and_edits_the_requested_line() {
    let mut session =
        ReviewSession::two_way("first\n界e\u{301}\nlast\n", "first\n界e\u{301}\nlast\n");
    session.go_to(Pane::Left, 1, 100);
    assert_eq!(session.cursor().line, 1);
    assert_eq!(session.cursor().grapheme, 2);

    key(&mut session, KeyCode::Char('i'));
    key(&mut session, KeyCode::Char('!'));
    key(&mut session, KeyCode::Esc);
    assert_eq!(session.pane_text(Pane::Left), "first\n界e\u{301}!\nlast\n");
}

#[test]
fn external_text_replacement_respects_read_only_and_is_one_undo_step() {
    let mut session = ReviewSession::two_way("old\n", "old\n");
    session.set_read_only(Pane::Left, true);
    assert_eq!(
        session.replace_pane_text(Pane::Left, "new\n"),
        Err(ReviewEditError::ReadOnly)
    );
    assert_eq!(session.pane_text(Pane::Left), "old\n");
    session.set_read_only(Pane::Left, false);
    let before = session.pane_snapshot(Pane::Left);
    session.replace_pane_text(Pane::Left, "new\n").unwrap();
    let after = session.pane_snapshot(Pane::Left);
    assert!(!before.same_identity(&after));
    assert_eq!(session.pane_text(Pane::Left), "new\n");

    key(&mut session, KeyCode::Char('u'));
    assert_eq!(session.pane_text(Pane::Left), "old\n");
    assert!(!before.same_identity(&session.pane_snapshot(Pane::Left)));
}

#[test]
fn undoing_a_conflict_choice_restores_unresolved_merge_state() {
    let mut session = ReviewSession::three_way("base\n", "ours\n", "theirs\n");
    key(&mut session, KeyCode::Char('t'));
    assert_eq!(session.pane_text(Pane::Result), "theirs\n");
    assert_eq!(session.unresolved_conflicts(), 0);

    key(&mut session, KeyCode::Char('u'));
    assert_eq!(session.pane_text(Pane::Result), "ours\n");
    assert_eq!(session.unresolved_conflicts(), 1);
    assert_eq!(
        key(&mut session, KeyCode::Char('s')),
        ReviewOutcome::UnresolvedConflicts(1)
    );
    ctrl_r(&mut session);
    assert_eq!(session.pane_text(Pane::Result), "theirs\n");
    assert_eq!(session.unresolved_conflicts(), 0);
}

#[test]
fn whitespace_policy_changes_matching_without_changing_reviewed_content() {
    let mut session = ReviewSession::two_way("a \n", "a\n");
    assert_eq!(session.hunk_count(), 1);
    session.set_whitespace_policy(WhitespacePolicy::IgnoreEdge);
    assert_eq!(session.hunk_count(), 0);
    assert_eq!(session.pane_text(Pane::Left), "a \n");
    assert_eq!(session.pane_text(Pane::Right), "a\n");
}

#[test]
fn accepted_generation_advances_only_when_clean_completion_is_displayed() {
    let mut session = ReviewSession::two_way("start\n", "start\n");
    let old = session.request_diff("start\n", "old\n");
    let latest = session.request_diff("start\n", "latest\n");
    let old_generation = old.generation();
    let latest_generation = latest.generation();
    let old_completion = old.compute();
    assert_eq!(old_completion.generation(), old_generation);
    assert_eq!(session.accepted_generation(), 0);
    session.handle(ReviewInput::DiffReady(latest.compute()));
    assert_eq!(session.accepted_generation(), latest_generation);
    session.handle(ReviewInput::DiffReady(old_completion));
    assert_eq!(session.accepted_generation(), latest_generation);
    assert_eq!(session.pane_text(Pane::Right), "latest\n");
}

#[test]
fn large_local_edits_defer_alignment_and_block_stale_apply_and_submit() {
    let original: String = (0..20_000).map(|line| format!("line{line:05}\n")).collect();
    let mut session = ReviewSession::two_way(&original, &original);
    session.go_to(Pane::Left, 10_000, 0);
    key(&mut session, KeyCode::Char('i'));
    key(&mut session, KeyCode::Char('X'));
    key(&mut session, KeyCode::Esc);

    assert!(session.is_local_diff_pending());
    assert_eq!(session.cursor().line, 10_000);
    assert_eq!(session.selected_hunk_ranges(), None);
    assert_eq!(
        key(&mut session, KeyCode::Char('s')),
        ReviewOutcome::LocalDiffPending
    );
    key(&mut session, KeyCode::Char('a'));
    assert_eq!(session.pane_text(Pane::Right), original);

    let deadline = Instant::now() + Duration::from_secs(10);
    while session.is_local_diff_pending() && Instant::now() < deadline {
        session.poll_background();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!session.is_local_diff_pending());
    assert_eq!(
        session.selected_hunk_ranges(),
        Some((10_000..10_001, 10_000..10_001))
    );
    assert_eq!(
        session.pane_text(Pane::Left).lines().nth(10_000),
        Some("Xline10000")
    );
    assert_eq!(
        session.pane_text(Pane::Right).lines().nth(10_000),
        Some("line10000")
    );
}

#[test]
fn undo_before_background_completion_cannot_restore_an_older_diff() {
    let original: String = (0..20_000).map(|line| format!("line{line:05}\n")).collect();
    let mut session = ReviewSession::two_way(&original, &original);
    session.go_to(Pane::Left, 10_000, 0);
    key(&mut session, KeyCode::Char('i'));
    key(&mut session, KeyCode::Char('X'));
    key(&mut session, KeyCode::Esc);
    key(&mut session, KeyCode::Char('u'));

    let deadline = Instant::now() + Duration::from_secs(10);
    while session.is_local_diff_pending() && Instant::now() < deadline {
        session.poll_background();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!session.is_local_diff_pending());
    assert_eq!(session.pane_text(Pane::Left), original);
    assert_eq!(session.hunk_count(), 0);
    assert!(matches!(
        key(&mut session, KeyCode::Char('s')),
        ReviewOutcome::Submitted(_)
    ));
}

#[test]
fn owned_snapshot_refresh_retains_generation_and_input_identity() {
    let mut session = ReviewSession::two_way("old\n", "old\n");
    let left = TextSnapshot::from_bytes(b"left\n").unwrap();
    let right = TextSnapshot::from_bytes(b"right\n").unwrap();
    let request = session.request_diff_snapshots(left.clone(), right.clone());
    let generation = request.generation();
    session.handle(ReviewInput::DiffReady(request.compute()));

    assert_eq!(session.accepted_generation(), generation);
    assert!(session.pane_snapshot(Pane::Left).same_identity(&left));
    assert!(session.pane_snapshot(Pane::Right).same_identity(&right));
}

#[test]
fn read_only_state_reflects_the_current_editable_pane() {
    let mut comparison = ReviewSession::two_way("left\n", "right\n");
    assert!(!comparison.is_read_only(Pane::Left));
    comparison.set_read_only(Pane::Right, true);
    assert!(comparison.is_read_only(Pane::Right));

    let merge = ReviewSession::three_way("base\n", "ours\n", "theirs\n");
    assert!(merge.is_read_only(Pane::Ours));
    assert!(!merge.is_read_only(Pane::Result));
    assert!(merge.is_read_only(Pane::Theirs));
}

#[test]
fn large_review_capture_preserves_current_identity_before_background_diff() {
    let original: String = (0..20_000).map(|line| format!("line{line:05}\n")).collect();
    let mut session = ReviewSession::two_way(&original, &original);
    let before = session.pane_snapshot(Pane::Left);
    session.go_to(Pane::Left, 10_000, 0);
    key(&mut session, KeyCode::Char('i'));
    key(&mut session, KeyCode::Char('X'));

    assert!(session.is_local_diff_pending());
    assert!(!session.pane_matches_snapshot(Pane::Left, &before));
    let captured = session.pane_capture(Pane::Left);
    let actual = captured.snapshot();
    assert!(session.pane_matches_snapshot(Pane::Left, &actual));
    assert_eq!(actual.text().lines().nth(10_000), Some("Xline10000"));
}
