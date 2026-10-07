use chvrn_tui::{Pane, ReviewInput, ReviewOutcome, ReviewSession};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn key(session: &mut ReviewSession, code: KeyCode) -> ReviewOutcome {
    session.handle(ReviewInput::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

fn session() -> ReviewSession {
    ReviewSession::three_way(
        "head\nanchor\ntail\n",
        "head\nimport café\nanchor\ntail\n",
        "head\nanchor\nimport café\ntail\n",
    )
}

#[test]
fn keeping_the_first_addition_removes_only_the_other_position_and_undo_restores_it() {
    let mut session = session();
    let original = "head\nimport café\nanchor\nimport café\ntail\n";
    assert_eq!(session.pane_text(Pane::Result), original);
    key(&mut session, KeyCode::Char('v'));
    key(&mut session, KeyCode::Char('k'));
    assert_eq!(
        session.pane_text(Pane::Result),
        "head\nimport café\nanchor\ntail\n"
    );
    key(&mut session, KeyCode::Char('u'));
    assert_eq!(session.pane_text(Pane::Result), original);
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Char('r'),
        KeyModifiers::CONTROL,
    )));
    assert_eq!(
        session.pane_text(Pane::Result),
        "head\nimport café\nanchor\ntail\n"
    );
}

#[test]
fn reviewing_the_other_position_can_keep_the_second_addition() {
    let mut session = session();
    key(&mut session, KeyCode::Char('v'));
    assert_eq!(session.cursor().line, 1);
    key(&mut session, KeyCode::Tab);
    assert_eq!(session.cursor().line, 3);
    key(&mut session, KeyCode::Char('k'));
    assert_eq!(
        session.pane_text(Pane::Result),
        "head\nanchor\nimport café\ntail\n"
    );
    assert_eq!(
        session.pane_text(Pane::Ours),
        "head\nimport café\nanchor\ntail\n"
    );
    assert_eq!(
        session.pane_text(Pane::Theirs),
        "head\nanchor\nimport café\ntail\n"
    );
}

#[test]
fn keeping_both_leaves_the_result_unchanged_and_does_not_prevent_submission() {
    let mut session = session();
    let original = session.pane_text(Pane::Result);
    key(&mut session, KeyCode::Char('v'));
    key(&mut session, KeyCode::Char('b'));
    assert_eq!(session.pane_text(Pane::Result), original);
    assert!(!session.is_dirty());
    key(&mut session, KeyCode::Char('s'));
    assert!(matches!(
        key(&mut session, KeyCode::Char('y')),
        ReviewOutcome::Submitted(_)
    ));
}

#[test]
fn pre_existing_repetition_is_not_offered_as_a_duplicate_addition() {
    let text = "head\nrepeat()\nanchor\nrepeat()\ntail\n";
    let mut session = ReviewSession::three_way(text, text, text);
    key(&mut session, KeyCode::Char('v'));
    assert!(!session.is_review_modal());
    key(&mut session, KeyCode::Char('k'));
    assert_eq!(session.pane_text(Pane::Result), text);
}

#[test]
fn cancelling_duplicate_review_cannot_accept_an_unrelated_conflict() {
    let mut session = ReviewSession::three_way(
        "head\nanchor\ntail\nold\n",
        "head\nimport café\nanchor\ntail\nours\n",
        "head\nanchor\nimport café\ntail\ntheirs\n",
    );
    key(&mut session, KeyCode::Char('v'));
    key(&mut session, KeyCode::Char('o'));
    assert_eq!(session.unresolved_conflicts(), 1);
    key(&mut session, KeyCode::Esc);
    assert!(!session.is_review_modal());
    assert_eq!(session.unresolved_conflicts(), 1);
}

#[test]
fn duplicate_resolution_preserves_line_endings_and_a_missing_final_newline() {
    for ending in ["\n", "\r\n", "\r"] {
        let base = ["head", "anchor", "tail"].join(ending);
        let ours = ["head", "import café", "anchor", "tail"].join(ending);
        let theirs = ["head", "anchor", "import café", "tail"].join(ending);
        let mut session = ReviewSession::three_way(&base, &ours, &theirs);
        key(&mut session, KeyCode::Char('v'));
        key(&mut session, KeyCode::Char('k'));
        assert_eq!(session.pane_text(Pane::Result), ours);
    }
}

#[test]
fn removing_multiple_extra_copies_is_one_undoable_decision() {
    let mut session = session();
    let edited = "head\nimport café\nanchor\nimport café\ntail\nimport café\n";
    session.replace_pane_text(Pane::Result, edited).unwrap();
    key(&mut session, KeyCode::Char('v'));
    key(&mut session, KeyCode::Char('k'));
    assert_eq!(
        session.pane_text(Pane::Result),
        "head\nimport café\nanchor\ntail\n"
    );
    key(&mut session, KeyCode::Char('u'));
    assert_eq!(session.pane_text(Pane::Result), edited);
}

#[test]
fn manually_removing_the_extra_copy_clears_the_duplicate_review() {
    let mut session = session();
    session
        .replace_pane_text(Pane::Result, "head\nimport café\nanchor\ntail\n")
        .unwrap();
    key(&mut session, KeyCode::Char('v'));
    assert!(!session.is_review_modal());
}
