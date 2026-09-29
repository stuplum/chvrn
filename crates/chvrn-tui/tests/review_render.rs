use chvrn_tui::{Pane, ReviewInput, ReviewSession};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use std::path::Path;

fn draw(session: &ReviewSession, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| session.render(frame)).unwrap();
    terminal.backend().buffer().clone()
}

fn visible_text(buffer: &Buffer) -> String {
    buffer.content.iter().map(|cell| cell.symbol()).collect()
}

fn cell_at(buffer: &Buffer, symbol: &str) -> (u16, u16) {
    let width = usize::from(buffer.area.width);
    let position = buffer
        .content
        .iter()
        .enumerate()
        .find(|(index, cell)| {
            let y = index / width;
            y >= 2 && y + 1 < usize::from(buffer.area.height) && cell.symbol() == symbol
        })
        .map(|(index, _)| index)
        .unwrap_or_else(|| panic!("missing visible editor cell {symbol}"));
    ((position % width) as u16, (position / width) as u16)
}

fn row_text(buffer: &Buffer, y: u16) -> String {
    (0..buffer.area.width)
        .map(|x| buffer[(x, y)].symbol())
        .collect()
}

#[test]
fn unequal_height_replacement_keeps_each_pane_continuous_and_joins_changed_regions() {
    let session = ReviewSession::two_way("head\nA-old\nB-old\nend\n", "head\nC-new\nend\n");
    let buffer = draw(&session, 80, 10);
    let (_, a_y) = cell_at(&buffer, "A");
    let (b_x, b_y) = cell_at(&buffer, "B");
    let (_, c_y) = cell_at(&buffer, "C");
    assert_eq!(b_y, a_y + 1);
    assert_eq!(c_y, a_y);
    assert!(row_text(&buffer, b_y).contains("end"));
    assert!(row_text(&buffer, b_y + 1).contains("end"));
    assert!(
        buffer
            .content
            .iter()
            .any(|cell| matches!(cell.symbol(), "▀" | "▄"))
    );
    assert_ne!(buffer[(b_x + 12, b_y)].bg, buffer[(b_x + 12, b_y + 1)].bg);
}

#[test]
fn insertion_edge_can_copy_the_empty_source_to_remove_the_inserted_lines() {
    let mut session = ReviewSession::two_way("head\nend\n", "head\nINSERT\nend\n");
    let buffer = draw(&session, 80, 8);
    let (_, insertion_y) = cell_at(&buffer, "I");
    assert!(row_text(&buffer, insertion_y).contains("end"));
    let (action_x, action_y) = cell_at(&buffer, "»");
    assert!(
        buffer
            .content
            .iter()
            .any(|cell| matches!(cell.symbol(), "▀" | "▄"))
    );
    session.handle(ReviewInput::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: action_x,
        row: action_y,
        modifiers: KeyModifiers::NONE,
    }));
    assert_eq!(session.pane_text(Pane::Right), "head\nend\n");
}

#[test]
fn unresolved_merge_shades_both_sources_and_result_differently_from_resolved_merge() {
    let mut session = ReviewSession::three_way(
        "head\nbase\ntail\n",
        "head\nλours\ntail\n",
        "head\nφtheirs\ntail\n",
    );
    let unresolved = draw(&session, 96, 10);
    let (source_x, y) = cell_at(&unresolved, "φ");
    let result_x = (0..source_x)
        .rev()
        .find(|x| unresolved[(*x, y)].symbol() == "λ")
        .unwrap();
    let unresolved_bg = unresolved[(result_x, y)].bg;
    assert_ne!(unresolved_bg, unresolved[(result_x, y + 1)].bg);

    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Char('o'),
        KeyModifiers::NONE,
    )));
    let resolved = draw(&session, 96, 10);
    let (_, resolved_y) = cell_at(&resolved, "φ");
    let resolved_x = (0..source_x)
        .rev()
        .find(|x| resolved[(*x, resolved_y)].symbol() == "λ")
        .unwrap();
    assert_ne!(unresolved_bg, resolved[(resolved_x, resolved_y)].bg);
    assert_eq!(session.unresolved_conflicts(), 0);
}

#[test]
fn changed_graphemes_have_visible_intraline_emphasis() {
    let session = ReviewSession::two_way("§alphaΩtail\n", "§alphaЖtail\n");
    let buffer = draw(&session, 80, 10);

    let (removed_x, removed_y) = cell_at(&buffer, "Ω");
    let (added_x, added_y) = cell_at(&buffer, "Ж");
    let (left_x, left_y) = cell_at(&buffer, "§");
    let right_x = (removed_x + 1..added_x)
        .find(|x| buffer[(*x, added_y)].symbol() == "§")
        .unwrap();
    assert_eq!(removed_y, added_y);
    assert_eq!(left_y, removed_y);
    assert_ne!(
        buffer[(removed_x, removed_y)].bg,
        buffer[(left_x, left_y)].bg
    );
    assert_ne!(buffer[(added_x, added_y)].bg, buffer[(right_x, added_y)].bg);
    assert_eq!(
        buffer[(removed_x, removed_y)].fg,
        buffer[(left_x, left_y)].fg
    );
    assert_eq!(buffer[(added_x, added_y)].fg, buffer[(right_x, added_y)].fg);
}

#[test]
fn wide_and_combining_graphemes_remain_visible_while_long_suffix_is_clipped() {
    let left = format!("界e\u{301}{}Ω\n", "x".repeat(120));
    let right = format!("界e\u{301}{}Ж\n", "x".repeat(120));
    let session = ReviewSession::two_way(&left, &right);
    let buffer = draw(&session, 48, 8);
    assert!(buffer.content.iter().any(|cell| cell.symbol() == "界"));
    assert!(
        buffer
            .content
            .iter()
            .any(|cell| cell.symbol() == "e\u{301}")
    );

    assert!(!visible_text(&buffer).contains('Ω'));
    assert!(!visible_text(&buffer).contains('Ж'));
}

#[test]
fn narrow_resize_keeps_the_focused_pane_visible_and_preserves_both_buffers() {
    let mut session = ReviewSession::two_way("LONLY\n", "RONLY\n");
    session.handle(ReviewInput::Resize {
        width: 18,
        height: 6,
    });
    let left_view = draw(&session, 18, 6);

    assert!(visible_text(&left_view).contains("LONLY"));
    assert!(!visible_text(&left_view).contains("RONLY"));
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Tab,
        KeyModifiers::NONE,
    )));
    let right_view = draw(&session, 18, 6);

    assert!(visible_text(&right_view).contains("RONLY"));
    assert!(!visible_text(&right_view).contains("LONLY"));
    assert_eq!(session.pane_text(Pane::Left), "LONLY\n");
    assert_eq!(session.pane_text(Pane::Right), "RONLY\n");
}

#[test]
fn narrow_merge_keeps_the_result_readable_and_switches_to_each_source() {
    let mut session = ReviewSession::three_way("base\n", "base\n", "theirs\n");
    session.handle(ReviewInput::Resize {
        width: 26,
        height: 6,
    });
    let result = draw(&session, 26, 6);
    assert!(visible_text(&result).contains("Merged result"));
    assert!(visible_text(&result).contains("theirs"));
    assert!(!visible_text(&result).contains("base"));

    for _ in 0..2 {
        session.handle(ReviewInput::Key(KeyEvent::new(
            KeyCode::Tab,
            KeyModifiers::NONE,
        )));
    }
    let ours = draw(&session, 26, 6);
    assert!(visible_text(&ours).contains("base"));
    assert!(!visible_text(&ours).contains("theirs"));
}

#[test]
fn clicking_a_directional_right_pane_control_applies_its_hunk() {
    let mut session = ReviewSession::two_way("top\nold\nend\n", "top\nnew\nend\n");
    session.handle(ReviewInput::Resize {
        width: 80,
        height: 8,
    });
    let initial = draw(&session, 80, 8);
    assert!(visible_text(&initial).contains("old"));
    assert!(visible_text(&initial).contains("new"));

    let (right_x, right_y) = cell_at(&initial, "«");
    session.handle(ReviewInput::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: right_x,
        row: right_y,
        modifiers: KeyModifiers::NONE,
    }));

    assert_eq!(session.pane_text(Pane::Left), "top\nnew\nend\n");
    assert_eq!(session.pane_text(Pane::Right), "top\nnew\nend\n");
}

#[test]
fn registered_rust_syntax_distinguishes_keyword_from_function_in_the_same_pane() {
    let mut session =
        ReviewSession::two_way("fn main() { let n = 1; }\n", "fn main() { let n = 2; }\n");
    session.set_paths(Path::new("main.rs"), Path::new("main.rs"));
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Tab,
        KeyModifiers::NONE,
    )));
    let buffer = draw(&session, 80, 8);
    let (keyword_x, y) = cell_at(&buffer, "f");
    let function_x = (keyword_x + 1..buffer.area.width)
        .find(|x| buffer[(*x, y)].symbol() == "m")
        .unwrap();
    assert_ne!(buffer[(keyword_x, y)].fg, buffer[(function_x, y)].fg);
}

#[test]
fn intraline_emphasis_retains_the_syntax_colour_of_a_changed_number() {
    let mut session = ReviewSession::two_way("fn main() { 1 + 3 }\n", "fn main() { 2 + 3 }\n");
    session.set_paths(Path::new("main.rs"), Path::new("main.rs"));
    let buffer = draw(&session, 80, 8);
    let (brace_x, y) = cell_at(&buffer, "{");
    let changed_x = (brace_x + 1..buffer.area.width / 2)
        .find(|x| buffer[(*x, y)].symbol() == "1")
        .unwrap();
    let unchanged_x = (changed_x + 1..buffer.area.width / 2)
        .find(|x| buffer[(*x, y)].symbol() == "3")
        .unwrap();
    assert_eq!(buffer[(changed_x, y)].fg, buffer[(unchanged_x, y)].fg);
    assert_ne!(buffer[(changed_x, y)].bg, buffer[(unchanged_x, y)].bg);
}

#[test]
fn overview_strips_indicate_offscreen_changes_without_scrolling_the_panes() {
    let original: String = (0..50).map(|line| format!("line{line:02}\n")).collect();
    let revised = original.replace("line45", "OFFSCREEN");
    let session = ReviewSession::two_way(&original, &revised);
    let buffer = draw(&session, 80, 11);
    assert!(visible_text(&buffer).contains("line00"));
    assert!(!visible_text(&buffer).contains("OFFSCREEN"));
    assert!(
        buffer
            .content
            .iter()
            .filter(|cell| cell.symbol() == "▐")
            .count()
            >= 2
    );
}

#[test]
fn tab_expands_to_display_cells_without_changing_the_source_text() {
    let session = ReviewSession::two_way("a\tb\n", "a\tc\n");
    let buffer = draw(&session, 80, 8);
    let (a, y) = cell_at(&buffer, "a");
    let b = (a + 1..buffer.area.width)
        .find(|x| buffer[(*x, y)].symbol() == "b")
        .unwrap();
    assert_eq!(b - a, 4);
    assert_eq!(session.pane_text(Pane::Left), "a\tb\n");
}

#[test]
fn pending_large_diff_displays_the_edited_line_while_matching_is_in_progress() {
    let original: String = (0..20_000).map(|line| format!("line{line:05}\n")).collect();
    let mut session = ReviewSession::two_way(&original, &original);
    session.handle(ReviewInput::Resize {
        width: 80,
        height: 8,
    });
    session.go_to(Pane::Left, 10_000, 0);
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Char('i'),
        KeyModifiers::NONE,
    )));
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Char('X'),
        KeyModifiers::NONE,
    )));
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Esc,
        KeyModifiers::NONE,
    )));
    let buffer = draw(&session, 80, 8);

    assert!(session.is_local_diff_pending());
    assert!(visible_text(&buffer).contains("Xline10000"));
}

#[test]
fn narrowing_a_merge_keeps_the_result_cursor_visible_on_a_long_line() {
    let text = format!("{}Ω\n", "a".repeat(150));
    let mut session = ReviewSession::three_way(&text, &text, &text);
    session.handle(ReviewInput::Resize {
        width: 180,
        height: 9,
    });
    session.go_to(Pane::Result, 0, 150);
    session.handle(ReviewInput::Resize {
        width: 38,
        height: 9,
    });
    let buffer = draw(&session, 38, 9);

    assert!(visible_text(&buffer).contains('Ω'));
    assert_eq!(session.cursor().grapheme, 150);
    assert_eq!(session.pane_text(Pane::Result), text);
}

#[test]
fn home_and_end_reveal_the_target_on_a_horizontally_clipped_line() {
    let text = format!("Ω{}Ж\n", "a".repeat(150));
    let mut session = ReviewSession::three_way(&text, &text, &text);
    session.handle(ReviewInput::Resize {
        width: 38,
        height: 9,
    });
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::End,
        KeyModifiers::NONE,
    )));
    assert_eq!(session.cursor().grapheme, 152);
    assert!(visible_text(&draw(&session, 38, 9)).contains('Ж'));

    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Home,
        KeyModifiers::NONE,
    )));
    assert_eq!(session.cursor().grapheme, 0);
    assert!(visible_text(&draw(&session, 38, 9)).contains('Ω'));
    assert_eq!(session.pane_text(Pane::Result), text);
}
