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
fn shift_tab_cycles_backwards_and_wraps_in_three_way_review() {
    for event in [
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT),
    ] {
        let mut session = ReviewSession::three_way("base\n", "ours\n", "theirs\n");
        assert_eq!(session.focus(), Pane::Result);
        for expected in [Pane::Ours, Pane::Theirs, Pane::Result] {
            session.handle(ReviewInput::Key(event));
            assert_eq!(
                session.focus(),
                expected,
                "incorrect reverse focus for {event:?}"
            );
        }
        key(&mut session, KeyCode::Tab);
        assert_eq!(session.focus(), Pane::Theirs);
        session.handle(ReviewInput::Key(event));
        assert_eq!(session.focus(), Pane::Result);
    }
}

#[test]
fn shift_tab_wraps_between_both_panes_in_two_way_review() {
    let mut session = ReviewSession::two_way("left\n", "right\n");
    for expected in [Pane::Right, Pane::Left] {
        session.handle(ReviewInput::Key(KeyEvent::new(
            KeyCode::BackTab,
            KeyModifiers::SHIFT,
        )));
        assert_eq!(session.focus(), expected);
    }
    key(&mut session, KeyCode::Tab);
    assert_eq!(session.focus(), Pane::Right);
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::BackTab,
        KeyModifiers::SHIFT,
    )));
    assert_eq!(session.focus(), Pane::Left);
}

#[test]
fn gutter_insertion_places_the_opposite_block_below_the_chosen_block() {
    for (choice, symbol, expected) in [
        ('o', "↙", "head\nours-a\nours-b\ntheirs\ntail\n"),
        ('t', "↘", "head\ntheirs\nours-a\nours-b\ntail\n"),
    ] {
        let mut session = ReviewSession::three_way(
            "head\nbase\ntail\n",
            "head\nours-a\nours-b\ntail\n",
            "head\ntheirs\ntail\n",
        );
        key(&mut session, KeyCode::Char(choice));
        assert_eq!(session.unresolved_conflicts(), 0);
        key(&mut session, KeyCode::Esc);

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
        key(&mut session, KeyCode::Char('s'));
        assert!(matches!(
            key(&mut session, KeyCode::Char('y')),
            ReviewOutcome::Submitted(submission) if submission.result.as_deref() == Some(expected)
        ));
    }
}

#[test]
fn asymmetric_choices_keep_complete_advice_and_remaining_source_insertion() {
    let mut session = ReviewSession::three_way("a\nb\n", "A\nb\n", "X\nY\n");
    session.set_merge_advice_enabled(true);
    let request = session.begin_merge_advice().unwrap();
    let ours = &request.input().ours;
    let captured: Vec<_> = ours
        .snapshot
        .text()
        .lines()
        .skip(ours.lines.start)
        .take(ours.lines.len())
        .collect();
    assert_eq!(captured, ["A", "b"]);
    session.cancel_merge_advice();
    key(&mut session, KeyCode::Char('t'));
    key(&mut session, KeyCode::Esc);
    click_gutter_control(&mut session, "↘");
    assert_eq!(session.pane_text(Pane::Result), "X\nY\nA\nb\n");
    assert_eq!(session.pane_text(Pane::Ours), "A\nb\n");
    assert_eq!(session.pane_text(Pane::Theirs), "X\nY\n");
    assert_eq!(session.unresolved_conflicts(), 0);
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
    key(&mut session, KeyCode::Esc);

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
    key(&mut session, KeyCode::Esc);
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

    click_gutter_control(&mut session, "↘");

    assert_eq!(
        session.pane_text(Pane::Result),
        "head\ntheirs-one\nλours-one-a\nours-one-b\nkeep\nstay\nours-two\ntail\n"
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
        "head\ntheirs-one\nλours-one-a\nours-one-b\nkeep\nstay\ntheirs-two-a\ntheirs-two-b\ntail\n"
    );
    assert_eq!(session.unresolved_conflicts(), 0);
    assert!(matches!(
        key(&mut session, KeyCode::Char('y')),
        ReviewOutcome::Submitted(submission)
            if submission.result.as_deref() == Some(
                "head\ntheirs-one\nλours-one-a\nours-one-b\nkeep\nstay\ntheirs-two-a\ntheirs-two-b\ntail\n"
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
    key(&mut session, KeyCode::Esc);
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
        ("head\ntheirs\ntail\n", "head\ntail\n", 't', "↘"),
    ] {
        let mut session = ReviewSession::three_way("head\nbase\ntail\n", ours, theirs);
        key(&mut session, KeyCode::Char(choice));
        assert_eq!(session.pane_text(Pane::Result), "head\ntail\n");
        assert_eq!(session.unresolved_conflicts(), 0);
        key(&mut session, KeyCode::Esc);

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
        assert_eq!(
            key(&mut session, KeyCode::Char(choice)),
            ReviewOutcome::Continue
        );
        assert_eq!(session.unresolved_conflicts(), 0);
        assert_eq!(session.pane_text(Pane::Result), expected);
        assert_eq!(session.pane_text(Pane::Ours), "head\nours\ntail\n");
        assert_eq!(session.pane_text(Pane::Theirs), "head\ntheirs\ntail\n");
        assert!(matches!(
            key(&mut session, KeyCode::Char('y')),
            ReviewOutcome::Submitted(submission) if submission.result.as_deref() == Some(expected)
        ));
    }
}

#[test]
fn merge_confirmation_is_offered_when_accepting_a_deletion_leaves_the_preview_unchanged() {
    let mut session =
        ReviewSession::three_way("head\nbase\ntail\n", "head\ntail\n", "head\ntheirs\ntail\n");
    assert_eq!(session.unresolved_conflicts(), 1);
    assert_eq!(session.pane_text(Pane::Result), "head\ntail\n");

    assert_eq!(
        key(&mut session, KeyCode::Char('o')),
        ReviewOutcome::Continue
    );
    assert_eq!(session.pane_text(Pane::Result), "head\ntail\n");
    assert!(matches!(
        key(&mut session, KeyCode::Char('y')),
        ReviewOutcome::Submitted(submission)
            if submission.result.as_deref() == Some("head\ntail\n")
    ));
}

#[test]
fn merge_confirmation_is_offered_after_resolving_the_final_conflict_with_the_mouse() {
    let mut session = ReviewSession::three_way("base\n", "ours\n", "theirs\n");

    assert_eq!(
        click_gutter_control(&mut session, "«"),
        ReviewOutcome::Continue
    );
    assert!(matches!(
        key(&mut session, KeyCode::Char('y')),
        ReviewOutcome::Submitted(submission)
            if submission.result.as_deref() == Some("theirs\n")
    ));
}

#[test]
fn merge_confirmation_cannot_accept_remaining_conflicts() {
    let mut session = ReviewSession::three_way(
        "head\nbase-one\nkeep\nstay\nbase-two\ntail\n",
        "head\nours-one\nkeep\nstay\nours-two\ntail\n",
        "head\ntheirs-one\nkeep\nstay\ntheirs-two\ntail\n",
    );
    assert_eq!(session.unresolved_conflicts(), 2);
    key(&mut session, KeyCode::Char('o'));

    assert_eq!(session.unresolved_conflicts(), 1);
    assert_eq!(
        key(&mut session, KeyCode::Char('s')),
        ReviewOutcome::UnresolvedConflicts(1)
    );
    assert_eq!(
        key(&mut session, KeyCode::Char('y')),
        ReviewOutcome::Continue
    );
    assert_eq!(session.unresolved_conflicts(), 1);

    assert_eq!(
        key(&mut session, KeyCode::Char('t')),
        ReviewOutcome::Continue
    );
    assert!(matches!(
        key(&mut session, KeyCode::Char('y')),
        ReviewOutcome::Submitted(submission)
            if submission.result.as_deref()
                == Some("head\nours-one\nkeep\nstay\ntheirs-two\ntail\n")
    ));
}

#[test]
fn merge_confirmation_can_be_cancelled_then_reopened_to_submit_further_edits() {
    for cancel in [KeyCode::Char('n'), KeyCode::Esc] {
        let mut session = ReviewSession::three_way("base\n", "ours\n", "theirs\n");
        key(&mut session, KeyCode::Char('o'));

        assert_eq!(key(&mut session, cancel), ReviewOutcome::Continue);
        assert_eq!(
            key(&mut session, KeyCode::Char('y')),
            ReviewOutcome::Continue
        );
        assert_eq!(session.pane_text(Pane::Result), "ours\n");
        assert_eq!(session.unresolved_conflicts(), 0);

        session.go_to(Pane::Result, 0, 0);
        key(&mut session, KeyCode::Char('i'));
        key(&mut session, KeyCode::Char('X'));
        key(&mut session, KeyCode::Esc);
        assert_eq!(session.pane_text(Pane::Result), "Xours\n");
        assert_eq!(
            key(&mut session, KeyCode::Char('s')),
            ReviewOutcome::Continue
        );
        assert!(matches!(
            key(&mut session, KeyCode::Char('y')),
            ReviewOutcome::Submitted(submission)
                if submission.result.as_deref() == Some("Xours\n")
        ));
    }
}

#[test]
fn merge_confirmation_requires_explicit_acceptance_of_an_automatically_merged_result() {
    let mut session = ReviewSession::three_way(
        "one\r\ntwo\r\nthree",
        "ONE\r\ntwo\r\nthree",
        "one\r\ntwo\r\nTHREE",
    );
    assert_eq!(session.unresolved_conflicts(), 0);

    assert_eq!(
        key(&mut session, KeyCode::Char('s')),
        ReviewOutcome::Continue
    );
    assert!(matches!(
        key(&mut session, KeyCode::Char('y')),
        ReviewOutcome::Submitted(submission)
            if submission.result.as_deref() == Some("ONE\r\ntwo\r\nTHREE")
    ));
}

#[test]
fn merge_confirmation_is_offered_again_after_undo_and_redo_of_the_final_choice() {
    let mut session = ReviewSession::three_way("base\n", "ours\n", "theirs\n");
    key(&mut session, KeyCode::Char('o'));
    key(&mut session, KeyCode::Esc);
    key(&mut session, KeyCode::Char('u'));

    assert_eq!(session.unresolved_conflicts(), 1);
    assert_eq!(
        key(&mut session, KeyCode::Char('s')),
        ReviewOutcome::UnresolvedConflicts(1)
    );
    assert_eq!(
        key(&mut session, KeyCode::Char('y')),
        ReviewOutcome::Continue
    );
    assert_eq!(ctrl_r(&mut session), ReviewOutcome::Continue);
    assert_eq!(session.unresolved_conflicts(), 0);
    assert!(matches!(
        key(&mut session, KeyCode::Char('y')),
        ReviewOutcome::Submitted(submission)
            if submission.result.as_deref() == Some("ours\n")
    ));
}

#[test]
fn merge_confirmation_does_not_submit_a_result_while_its_latest_alignment_is_pending() {
    let original: String = (0..20_000).map(|line| format!("line{line:05}\n")).collect();
    let mut session = ReviewSession::three_way(&original, &original, &original);
    assert!(matches!(
        key(&mut session, KeyCode::Char('s')),
        ReviewOutcome::Continue
    ));

    let revised = original.replace("line10000", "reviewed result");
    session.replace_pane_text(Pane::Result, &revised).unwrap();
    assert!(session.is_local_diff_pending());
    assert!(matches!(
        key(&mut session, KeyCode::Char('y')),
        ReviewOutcome::LocalDiffPending
    ));
    assert!(matches!(
        key(&mut session, KeyCode::Char('s')),
        ReviewOutcome::LocalDiffPending
    ));

    let deadline = Instant::now() + Duration::from_secs(10);
    while session.is_local_diff_pending() && Instant::now() < deadline {
        session.poll_background();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!session.is_local_diff_pending());
    assert!(matches!(
        key(&mut session, KeyCode::Char('s')),
        ReviewOutcome::Continue
    ));
    assert!(matches!(
        key(&mut session, KeyCode::Char('y')),
        ReviewOutcome::Submitted(submission)
            if submission.result.as_deref() == Some(revised.as_str())
    ));
}

#[test]
fn merge_confirmation_prevents_edit_keys_and_paste_from_changing_the_result() {
    let mut session = ReviewSession::three_way("base\n", "ours\n", "theirs\n");
    key(&mut session, KeyCode::Char('o'));

    key(&mut session, KeyCode::Char('i'));
    session.handle(ReviewInput::Paste("accidental edit\n".to_owned()));
    key(&mut session, KeyCode::Char('t'));
    assert_eq!(session.pane_text(Pane::Result), "ours\n");
    assert!(matches!(
        key(&mut session, KeyCode::Char('y')),
        ReviewOutcome::Submitted(submission)
            if submission.result.as_deref() == Some("ours\n")
    ));
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
    assert_eq!(
        key(&mut session, KeyCode::Char('r')),
        ReviewOutcome::Continue
    );

    assert_eq!(session.unresolved_conflicts(), 0);
    assert!(matches!(
        key(&mut session, KeyCode::Char('y')),
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

    session.handle(ReviewInput::DiffReady(newer.compute()));
    assert_eq!(session.pane_text(Pane::Right), "latest candidate\n");
    session.handle(ReviewInput::DiffReady(older.compute()));
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
    session.handle(ReviewInput::DiscardAndReload);
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
    key(&mut session, KeyCode::Esc);

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
fn undoing_a_deletion_choice_restores_all_four_conflicts_and_their_controls() {
    let base = "head\nbase-one\nkeep-one\nstay-one\nbase-two\nkeep-two\nstay-two\ndeleted\nkeep-three\nstay-three\nbase-four\ntail\n";
    let ours = "head\nours-one\nkeep-one\nstay-one\nours-two\nkeep-two\nstay-two\nkeep-three\nstay-three\nours-four\ntail\n";
    let theirs = "head\ntheirs-one\nkeep-one\nstay-one\ntheirs-two\nkeep-two\nstay-two\nedited\nkeep-three\nstay-three\ntheirs-four\ntail\n";
    let mut session = ReviewSession::three_way(base, ours, theirs);
    assert_eq!(session.unresolved_conflicts(), 4);
    key(&mut session, KeyCode::Char(']'));
    key(&mut session, KeyCode::Char(']'));
    key(&mut session, KeyCode::Char('o'));
    assert_eq!(session.unresolved_conflicts(), 3);
    assert_eq!(session.pane_text(Pane::Result), ours);

    key(&mut session, KeyCode::Char('u'));
    assert_eq!(
        key(&mut session, KeyCode::Char('s')),
        ReviewOutcome::UnresolvedConflicts(4),
    );
    assert_eq!(session.pane_text(Pane::Result), ours);
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| session.render(frame)).unwrap();
    let controls = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .filter(|cell| matches!(cell.symbol(), "»" | "«"))
        .count();
    assert_eq!(controls, 8);

    ctrl_r(&mut session);
    assert_eq!(session.unresolved_conflicts(), 3);
    assert_eq!(session.pane_text(Pane::Result), ours);
    key(&mut session, KeyCode::Char('u'));
    assert_eq!(session.unresolved_conflicts(), 4);
}

#[test]
fn undoing_a_deletion_choice_preserves_an_earlier_text_changing_resolution() {
    let base = "head\nbase-one\nkeep\nstay\ndeleted\ntail\n";
    let ours = "head\nours-one\nkeep\nstay\ntail\n";
    let theirs = "head\ntheirs-one\nkeep\nstay\nedited\ntail\n";
    let first_resolved = "head\ntheirs-one\nkeep\nstay\ntail\n";
    let mut session = ReviewSession::three_way(base, ours, theirs);
    key(&mut session, KeyCode::Char('t'));
    assert_eq!(session.pane_text(Pane::Result), first_resolved);
    assert_eq!(session.unresolved_conflicts(), 1);
    key(&mut session, KeyCode::Char('o'));
    assert_eq!(session.unresolved_conflicts(), 0);
    key(&mut session, KeyCode::Esc);

    key(&mut session, KeyCode::Char('u'));
    assert_eq!(session.pane_text(Pane::Result), first_resolved);
    assert_eq!(session.unresolved_conflicts(), 1);
    key(&mut session, KeyCode::Char('u'));
    assert_eq!(session.pane_text(Pane::Result), ours);
    assert_eq!(session.unresolved_conflicts(), 2);

    ctrl_r(&mut session);
    assert_eq!(session.pane_text(Pane::Result), first_resolved);
    assert_eq!(session.unresolved_conflicts(), 1);
    ctrl_r(&mut session);
    assert_eq!(session.pane_text(Pane::Result), first_resolved);
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

#[test]
fn foreign_refresh_cannot_replace_an_already_accepted_snapshot() {
    let mut first = ReviewSession::two_way("first\n", "first\n");
    let foreign = first.request_diff("first\n", "foreign\n");
    let mut second = ReviewSession::two_way("second\n", "second\n");
    let own = second.request_diff("second\n", "current\n");
    second.handle(ReviewInput::DiffReady(own.compute()));
    let current = second.pane_snapshot(Pane::Right);

    second.handle(ReviewInput::DiffReady(foreign.compute()));

    assert_eq!(second.pane_text(Pane::Right), "current\n");
    assert!(second.pane_matches_snapshot(Pane::Right, &current));
}

#[test]
fn whitespace_change_during_refresh_installs_incoming_bytes_under_current_policy() {
    let mut session = ReviewSession::two_way("old\n", "old\n");
    let request = session.request_diff("incoming \n", "incoming\n");
    session.set_whitespace_policy(WhitespacePolicy::IgnoreEdge);

    session.handle(ReviewInput::DiffReady(request.compute()));

    assert_eq!(session.pane_text(Pane::Left), "incoming \n");
    assert_eq!(session.pane_text(Pane::Right), "incoming\n");
    assert_eq!(session.hunk_count(), 0);
    assert!(matches!(
        key(&mut session, KeyCode::Char('s')),
        ReviewOutcome::Submitted(submission)
            if submission.left == "incoming \n" && submission.right == "incoming\n"
    ));
}

#[test]
fn whitespace_change_preserves_explicit_discard_for_dirty_refresh() {
    let mut session = ReviewSession::two_way("old\n", "old\n");
    let request = session.request_diff("incoming \n", "incoming\n");
    key(&mut session, KeyCode::Char('i'));
    session.handle(ReviewInput::Paste("local ".into()));
    key(&mut session, KeyCode::Esc);
    session.set_whitespace_policy(WhitespacePolicy::IgnoreEdge);

    assert_eq!(
        session.handle(ReviewInput::DiffReady(request.compute())),
        ReviewOutcome::RefreshConflict
    );
    assert_eq!(session.pane_text(Pane::Left), "local old\n");
    session.handle(ReviewInput::DiscardAndReload);
    assert_eq!(session.pane_text(Pane::Left), "incoming \n");
    assert_eq!(session.pane_text(Pane::Right), "incoming\n");
    assert_eq!(session.hunk_count(), 0);
}

#[test]
fn editing_displayed_line_treats_unicode_separators_as_content() {
    for separator in ['\u{000b}', '\u{000c}', '\u{0085}', '\u{2028}', '\u{2029}'] {
        let original = format!("a{separator}b\nc");
        let mut session = ReviewSession::two_way(&original, &original);
        session.go_to(Pane::Left, 1, 0);
        key(&mut session, KeyCode::Char('i'));
        session.handle(ReviewInput::Paste("X".into()));
        assert_eq!(
            session.pane_text(Pane::Left),
            format!("a{separator}b\nXc"),
            "separator {separator:?}"
        );
    }
}
