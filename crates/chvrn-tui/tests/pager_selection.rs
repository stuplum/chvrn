use chvrn_core::unified::UnifiedPatch;
use chvrn_tui::{PagerSession, Theme};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Point {
    column: u16,
    row: u16,
}

#[derive(Clone, Copy)]
struct TextCells {
    start: Point,
    end: Point,
}

fn session(source: &str) -> PagerSession {
    PagerSession::new(
        UnifiedPatch::parse(source.to_owned()).unwrap(),
        Arc::new(Theme::default()),
    )
}

fn draw(pager: &mut PagerSession, width: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, 18)).unwrap();
    terminal.draw(|frame| pager.render(frame)).unwrap();
    terminal.backend().buffer().clone()
}

fn text_cells(buffer: &Buffer, text: &str) -> TextCells {
    assert!(text.is_ascii() && !text.is_empty());
    let symbols = text.as_bytes();
    let mut found = None;
    for row in buffer.area.y..buffer.area.bottom() {
        for column in buffer.area.x..buffer.area.right() {
            if usize::from(buffer.area.right() - column) < symbols.len() {
                break;
            }
            if symbols.iter().enumerate().all(|(offset, symbol)| {
                buffer[(column + offset as u16, row)].symbol().as_bytes()
                    == std::slice::from_ref(symbol)
            }) {
                assert!(found.is_none(), "ambiguous visible occurrence of {text:?}");
                found = Some(TextCells {
                    start: Point { column, row },
                    end: Point {
                        column: column + symbols.len() as u16,
                        row,
                    },
                });
            }
        }
    }
    found.unwrap_or_else(|| panic!("missing visible occurrence of {text:?}"))
}

fn source_row(buffer: &Buffer, token: &str) -> String {
    let row = text_cells(buffer, token).start.row;
    (buffer.area.x..buffer.area.right())
        .map(|column| buffer[(column, row)].symbol())
        .collect()
}

fn key(pager: &mut PagerSession, code: KeyCode, modifiers: KeyModifiers) {
    assert!(!pager.handle(Event::Key(KeyEvent::new(code, modifiers))));
}

fn mouse(pager: &mut PagerSession, kind: MouseEventKind, point: Point) {
    assert!(!pager.handle(Event::Mouse(MouseEvent {
        kind,
        column: point.column,
        row: point.row,
        modifiers: KeyModifiers::NONE,
    })));
}

fn select(pager: &mut PagerSession, start: Point, end: Point) {
    mouse(pager, MouseEventKind::Down(MouseButton::Left), start);
    mouse(pager, MouseEventKind::Drag(MouseButton::Left), end);
    mouse(pager, MouseEventKind::Up(MouseButton::Left), end);
}

fn copy(pager: &mut PagerSession) -> Option<String> {
    key(pager, KeyCode::Char('c'), KeyModifiers::CONTROL);
    pager.take_copy_request()
}

const PAIRED: &str = "--- a/example.txt\n+++ b/example.txt\n@@ -40,3 +70,3 @@\n-old_alpha!\n-old_middle\n-old_omega?\n+new_beta!\n+new_middle\n+new_sigma?\n";

#[test]
fn copying_each_pane_emits_only_selected_source_and_leaves_the_patch_read_only() {
    let mut pager = session(PAIRED);
    let before = draw(&mut pager, 140);
    for (token, expected) in [("old_alpha", "old_alpha"), ("new_beta", "new_beta")] {
        let cells = text_cells(&before, token);
        select(&mut pager, cells.start, cells.end);
        assert_eq!(copy(&mut pager).as_deref(), Some(expected));
        assert_eq!(pager.take_copy_request(), None);
        let after = draw(&mut pager, 140);
        for source in ["old_alpha!", "old_middle", "old_omega?"] {
            assert_eq!(source_row(&after, source), source_row(&before, source));
        }
    }
}

#[test]
fn multiline_copy_excludes_gutters_change_markers_and_the_opposite_pane() {
    let mut pager = session(PAIRED);
    let buffer = draw(&mut pager, 140);
    for (first, last, expected) in [
        (
            "old_alpha!",
            "old_omega",
            "old_alpha!\nold_middle\nold_omega",
        ),
        ("new_beta!", "new_sigma", "new_beta!\nnew_middle\nnew_sigma"),
    ] {
        select(
            &mut pager,
            text_cells(&buffer, first).start,
            text_cells(&buffer, last).end,
        );
        assert_eq!(copy(&mut pager).as_deref(), Some(expected));
    }
}

#[test]
fn backwards_multiline_selection_copies_in_source_order() {
    let mut pager = session(PAIRED);
    let buffer = draw(&mut pager, 140);
    select(
        &mut pager,
        text_cells(&buffer, "old_omega").end,
        text_cells(&buffer, "alpha!").start,
    );
    assert_eq!(
        copy(&mut pager).as_deref(),
        Some("alpha!\nold_middle\nold_omega")
    );
}

#[test]
fn copy_preserves_tabs_wide_characters_and_combining_graphemes_as_source_bytes() {
    let mut pager = session(
        "--- a/unicode.txt\n+++ b/unicode.txt\n@@ -1 +1 @@\n-OLD[\t界e\u{301}]old_stop\n+NEW[\t語a\u{308}]new_stop\n",
    );
    let buffer = draw(&mut pager, 140);
    for (first, boundary, expected) in [
        ("OLD[", "]old_stop", "OLD[\t界e\u{301}"),
        ("NEW[", "]new_stop", "NEW[\t語a\u{308}"),
    ] {
        select(
            &mut pager,
            text_cells(&buffer, first).start,
            text_cells(&buffer, boundary).start,
        );
        assert_eq!(copy(&mut pager).as_deref(), Some(expected));
    }
}

#[test]
fn horizontally_scrolled_selection_maps_visible_cells_to_original_source() {
    let old = format!("{}old_target!", "x".repeat(100));
    let new = format!("{}new_target!", "y".repeat(100));
    let mut pager = session(&format!(
        "--- a/long.txt\n+++ b/long.txt\n@@ -1 +1 @@\n-{old}\n+{new}\n"
    ));
    draw(&mut pager, 100);
    for _ in 0..90 {
        key(&mut pager, KeyCode::Right, KeyModifiers::NONE);
    }
    let buffer = draw(&mut pager, 100);
    for token in ["old_target", "new_target"] {
        let cells = text_cells(&buffer, token);
        select(&mut pager, cells.start, cells.end);
        assert_eq!(copy(&mut pager).as_deref(), Some(token));
    }
}

#[test]
fn narrow_pager_copies_the_visible_focused_source_on_both_sides() {
    let mut pager = session(PAIRED);
    let right = draw(&mut pager, 45);
    let cells = text_cells(&right, "new_beta");
    select(&mut pager, cells.start, cells.end);
    assert_eq!(copy(&mut pager).as_deref(), Some("new_beta"));
    key(&mut pager, KeyCode::Tab, KeyModifiers::NONE);
    let left = draw(&mut pager, 45);
    let cells = text_cells(&left, "old_alpha");
    select(&mut pager, cells.start, cells.end);
    assert_eq!(copy(&mut pager).as_deref(), Some("old_alpha"));
}

#[test]
fn empty_selection_emits_no_request_to_overwrite_the_clipboard() {
    let mut pager = session(PAIRED);
    let buffer = draw(&mut pager, 140);
    assert_eq!(copy(&mut pager), None);
    let cells = text_cells(&buffer, "old_alpha");
    select(&mut pager, cells.start, cells.end);
    assert_eq!(copy(&mut pager).as_deref(), Some("old_alpha"));
    select(&mut pager, cells.start, cells.start);
    assert_eq!(copy(&mut pager), None);
    assert_eq!(pager.take_copy_request(), None);
}

#[test]
fn file_navigation_clears_selection_instead_of_copying_stale_source() {
    let mut pager = session(&format!(
        "{PAIRED}diff --git a/second.txt b/second.txt\n--- a/second.txt\n+++ b/second.txt\n@@ -1 +1 @@\n-old_second!\n+new_second!\n"
    ));
    let first = draw(&mut pager, 140);
    let cells = text_cells(&first, "old_alpha");
    select(&mut pager, cells.start, cells.end);
    assert_eq!(copy(&mut pager).as_deref(), Some("old_alpha"));
    key(&mut pager, KeyCode::Char('n'), KeyModifiers::CONTROL);
    let second = draw(&mut pager, 140);
    let cells = text_cells(&second, "new_second");
    assert_eq!(copy(&mut pager), None);
    select(&mut pager, cells.start, cells.end);
    assert_eq!(copy(&mut pager).as_deref(), Some("new_second"));
    key(&mut pager, KeyCode::Char('p'), KeyModifiers::CONTROL);
    draw(&mut pager, 140);
    assert_eq!(copy(&mut pager), None);
}

#[test]
fn dragging_across_the_divider_stays_in_the_starting_pane() {
    let mut pager = session(PAIRED);
    let buffer = draw(&mut pager, 140);
    let left = text_cells(&buffer, "old_alpha!");
    let right = text_cells(&buffer, "new_beta");
    assert_eq!(left.start.row, right.start.row);
    select(&mut pager, left.start, right.end);
    assert_eq!(copy(&mut pager).as_deref(), Some("old_alpha!"));
    select(&mut pager, right.end, left.start);
    assert_eq!(copy(&mut pager).as_deref(), Some("new_beta"));
}

#[test]
fn sparse_hunk_selection_stops_before_omitted_source() {
    let mut pager = session(
        "--- a/sparse.txt\n+++ b/sparse.txt\n@@ -1 +1 @@\n-old_first!\n+new_first!\n@@ -100 +100 @@\n-old_last!\n+new_last!\n",
    );
    let buffer = draw(&mut pager, 140);
    let first = text_cells(&buffer, "new_first!");
    let last = text_cells(&buffer, "new_last!");
    select(&mut pager, first.start, last.end);
    assert_eq!(copy(&mut pager).as_deref(), Some("new_first!"));
    select(&mut pager, last.end, first.start);
    assert_eq!(copy(&mut pager).as_deref(), Some("new_last!"));
}

#[test]
fn selection_is_visible_and_escape_clears_it_before_quitting() {
    let mut pager = session(PAIRED);
    let before = draw(&mut pager, 140);
    let cells = text_cells(&before, "new_beta");
    select(&mut pager, cells.start, cells.end);
    let selected = draw(&mut pager, 140);
    let at = (cells.start.column, cells.start.row);
    assert_ne!(before[at].style(), selected[at].style());
    key(&mut pager, KeyCode::Esc, KeyModifiers::NONE);
    let cleared = draw(&mut pager, 140);
    assert_eq!(before[at].style(), cleared[at].style());
    assert_eq!(copy(&mut pager), None);
    assert!(pager.handle(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))));
}

#[test]
fn paste_and_editing_keys_do_not_mutate_selected_source() {
    let mut pager = session(PAIRED);
    let buffer = draw(&mut pager, 140);
    let cells = text_cells(&buffer, "new_beta");
    select(&mut pager, cells.start, cells.end);
    assert!(!pager.handle(Event::Paste("replacement".into())));
    for code in [KeyCode::Delete, KeyCode::Backspace, KeyCode::Char('x')] {
        key(&mut pager, code, KeyModifiers::NONE);
    }
    assert_eq!(copy(&mut pager).as_deref(), Some("new_beta"));
    let after = draw(&mut pager, 140);
    assert_eq!(
        source_row(&after, "new_beta!"),
        source_row(&buffer, "new_beta!")
    );
}
