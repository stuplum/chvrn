use chvrn_tui::{Pane, ReviewInput, ReviewSession};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use std::path::Path;

fn external_theme(source: &str) -> std::sync::Arc<chvrn_tui::Theme> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "chvrn-render-theme-{}-{}.toml",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::write(&path, source).unwrap();
    let theme = chvrn_tui::Theme::load(path.to_str().unwrap(), None).unwrap();
    std::fs::remove_file(path).unwrap();
    std::sync::Arc::new(theme)
}

#[test]
fn switching_theme_repaints_without_changing_content_cursor_or_edit_history() {
    use ratatui::style::Color;
    let mut session = ReviewSession::two_way("same\nold\n", "same\nnew\n");
    let before = draw(&session, 100, 12);
    let cursor = session.cursor();
    let selected = session.selected_hunk();
    session.set_theme(external_theme(
        r##"
"ui.background" = { bg = "#fafafa" }
"ui.text" = { fg = "#202020" }
"ui.statusline" = { fg = "#303030", bg = "#e0e0e0" }
"ui.linenr" = "#606060"
"diff.plus" = "#008000"
"diff.minus" = "#a00000"
"diff.delta" = "#0000a0"
"ui.help" = { fg = "#101010", bg = "#dddddd" }
"ui.cursor" = { fg = "#ffffff", bg = "#000000" }
"chvrn.rail" = { bg = "#cccccc" }
"chvrn.action" = { fg = "#ff00ff" }
        "##,
    ));
    let after = draw(&session, 100, 12);
    assert_eq!(visible_text(&before), visible_text(&after));
    assert_eq!(cursor, session.cursor());
    assert_eq!(selected, session.selected_hunk());
    assert!(!session.is_dirty());
    assert_eq!(after[(0, 0)].bg, Color::Rgb(224, 224, 224));
    assert_eq!(after[(0, 11)].bg, Color::Rgb(224, 224, 224));
    assert_eq!(after[(5, 9)].bg, Color::Rgb(250, 250, 250));
    let action = cell_at(&after, "»");
    assert_eq!(after[action].fg, Color::Rgb(255, 0, 255));
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Char('a'),
        KeyModifiers::NONE,
    )));
    assert_eq!(session.pane_text(Pane::Right), "same\nold\n");
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Char('u'),
        KeyModifiers::NONE,
    )));
    assert_eq!(session.pane_text(Pane::Right), "same\nnew\n");
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Char('?'),
        KeyModifiers::NONE,
    )));
    let help = draw(&session, 100, 12);
    assert!(help.content.iter().any(|cell| cell.symbol() == "H"
        && cell.fg == Color::Rgb(16, 16, 16)
        && cell.bg == Color::Rgb(221, 221, 221)));
}

#[test]
fn syntax_modifiers_survive_diff_and_explicit_inline_overrides() {
    use ratatui::style::{Color, Modifier};
    let mut session = ReviewSession::two_way("let value = 1;\n", "let value = 2;\n");
    session.set_paths(Path::new("left.rs"), Path::new("right.rs"));
    session.set_theme(external_theme(
        r##"
"ui.background" = { bg = "#ffffff" }
"ui.text" = { fg = "#202020" }
"ui.selection" = { fg = "#444444", bg = "#bbbbbb" }
"keyword" = { fg = "#800080", modifiers = ["italic"] }
"constant.numeric" = { fg = "#008080", modifiers = ["underlined"] }
"chvrn.diff.modified.old" = { bg = "#eeeeee" }
"chvrn.diff.modified.old.selected" = { bg = "#dddddd" }
"chvrn.diff.modified.old.selected.inline" = { bg = "#cccccc" }
        "##,
    ));
    let buffer = draw(&session, 100, 12);
    let keyword = cell_at(&buffer, "e");
    assert_eq!(buffer[keyword].fg, Color::Rgb(128, 0, 128));
    assert!(buffer[keyword].modifier.contains(Modifier::ITALIC));
    assert_eq!(buffer[keyword].bg, Color::Rgb(221, 221, 221));
    let number = buffer
        .content
        .iter()
        .find(|cell| cell.symbol() == "1" && cell.modifier.contains(Modifier::UNDERLINED))
        .expect("changed number retains syntax underline");
    assert_eq!(number.bg, Color::Rgb(204, 204, 204));
    assert!(number.modifier.contains(Modifier::BOLD));
}

#[test]
fn derived_diff_backgrounds_follow_light_and_dark_surfaces() {
    use ratatui::style::Color;
    for (surface, light) in [("#ffffff", true), ("#101010", false)] {
        let mut session = ReviewSession::two_way("prefix old suffix\n", "prefix new suffix\n");
        session.set_theme(external_theme(&format!(
            "\"ui.background\" = {{ bg = \"{surface}\" }}\n\"diff.delta\" = \"#2040a0\"\n"
        )));
        let buffer = draw(&session, 100, 10);
        let unchanged = cell_at(&buffer, "r");
        let changed = cell_at(&buffer, "o");
        let brightness = |color| match color {
            Color::Rgb(r, g, b) => u16::from(r) + u16::from(g) + u16::from(b),
            other => panic!("expected derived RGB background, got {other:?}"),
        };
        assert_ne!(buffer[unchanged].bg, buffer[changed].bg);
        if light {
            assert!(brightness(buffer[unchanged].bg) > brightness(buffer[changed].bg));
            assert!(brightness(buffer[unchanged].bg) > 500);
        } else {
            assert!(brightness(buffer[unchanged].bg) < brightness(buffer[changed].bg));
            assert!(brightness(buffer[unchanged].bg) < 300);
        }
    }
}

#[test]
fn conflict_overrides_cover_selected_result_connectors_actions_and_overview() {
    use ratatui::style::Color;
    let mut session = ReviewSession::three_way("base\n", "ours\n", "theirs\n");
    session.set_theme(external_theme(
        r##"
"ui.background" = { bg = "#f0f0f0" }
"ui.text" = { fg = "#202020" }
"ui.cursor" = { fg = "#010203", bg = "#040506" }
"chvrn.diff.conflict.result.selected" = { bg = "#aabbcc" }
"chvrn.diff.conflict.result.selected.inline" = { bg = "#bbccdd" }
"chvrn.connector.conflict" = { bg = "#778899" }
"chvrn.action.conflict" = { fg = "#112233", bg = "#445566" }
"chvrn.overview.conflict.viewport" = { fg = "#123456", bg = "#654321" }
        "##,
    ));
    let before = draw(&session, 140, 12);
    assert!(
        before
            .content
            .iter()
            .any(|cell| cell.bg == Color::Rgb(170, 187, 204))
    );
    assert!(
        before
            .content
            .iter()
            .any(|cell| cell.bg == Color::Rgb(119, 136, 153))
    );
    let action = cell_at(&before, "»");
    assert_eq!(before[action].fg, Color::Rgb(17, 34, 51));
    assert_eq!(before[action].bg, Color::Rgb(68, 85, 102));
    assert!(before.content.iter().any(|cell| cell.symbol() == "█"
        && cell.fg == Color::Rgb(18, 52, 86)
        && cell.bg == Color::Rgb(101, 67, 33)));
    assert!(
        before
            .content
            .iter()
            .any(|cell| cell.fg == Color::Rgb(1, 2, 3) && cell.bg == Color::Rgb(4, 5, 6))
    );
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Char('o'),
        KeyModifiers::NONE,
    )));
    assert_eq!(session.unresolved_conflicts(), 0);
    assert_eq!(session.pane_text(Pane::Result), "ours\n");
    let resolved = draw(&session, 140, 12);
    assert!(
        !resolved
            .content
            .iter()
            .any(|cell| cell.bg == Color::Rgb(170, 187, 204))
    );
}

fn overlapping_insertion_session(ours: &str, theirs: &str) -> ReviewSession {
    let mut session = ReviewSession::three_way("head\ntail\n", ours, theirs);
    session.set_theme(external_theme(
        r##"
"ui.background" = { bg = "#101010" }
"chvrn.rail" = { bg = "#202020" }
"chvrn.diff.conflict.selected" = { bg = "#aabbcc" }
"chvrn.diff.conflict.result.selected" = { bg = "#aabbcc" }
"chvrn.connector.conflict" = { bg = "#778899" }
"chvrn.action.conflict" = { bg = "#778899" }
        "##,
    ));
    session
}

#[test]
fn asymmetric_conflicts_shade_unchanged_lines_inside_the_complete_choice() {
    use ratatui::style::Color;
    let mut session = ReviewSession::three_way("a\nb\n", "A\nb\n", "X\nY\n");
    session.set_theme(external_theme(
        r##"
"ui.background" = { bg = "#101010" }
"chvrn.diff.conflict.selected" = { bg = "#aabbcc" }
"chvrn.diff.conflict.result.selected" = { bg = "#aabbcc" }
        "##,
    ));
    let buffer = draw(&session, 180, 12);
    let mut unchanged_choice_lines = 0;
    for y in 2..6 {
        for x in 0..buffer.area.width {
            if buffer[(x, y)].symbol() == "b" {
                unchanged_choice_lines += 1;
                assert_eq!(buffer[(x + 12, y)].bg, Color::Rgb(170, 187, 204));
            }
        }
    }
    assert_eq!(unchanged_choice_lines, 2);
}

#[test]
fn shared_lines_in_competing_insertions_are_shaded_as_part_of_the_whole_conflict() {
    use ratatui::style::Color;
    let short = "head\nαshared\nβshared\ntail\n";
    let long = "head\nαshared\nβshared\nγextra\ntail\n";
    for (ours, theirs) in [(long, short), (short, long)] {
        let session = overlapping_insertion_session(ours, theirs);
        assert_eq!(session.unresolved_conflicts(), 1);
        let buffer = draw(&session, 180, 12);
        let mut shared_lines = 0;
        for y in 2..8 {
            for x in 0..buffer.area.width {
                if matches!(buffer[(x, y)].symbol(), "α" | "β" | "γ") {
                    shared_lines += usize::from(matches!(buffer[(x, y)].symbol(), "α" | "β"));
                    assert_eq!(
                        buffer[(x + 12, y)].bg,
                        Color::Rgb(170, 187, 204),
                        "conflict source is unshaded at ({x}, {y}) for {ours:?} / {theirs:?}",
                    );
                }
            }
        }
        assert_eq!(
            shared_lines, 6,
            "both shared lines must appear in all three panes"
        );
    }
}

#[test]
fn conflict_connector_edges_cover_the_complete_choices_without_spilling_into_context() {
    use ratatui::style::Color;
    let short = "head\nαshared\nβshared\ntail\n";
    let long = "head\nαshared\nβshared\nγextra\ntail\n";
    for (ours, theirs, ours_height, theirs_height) in [(long, short, 3, 2), (short, long, 2, 3)] {
        let session = overlapping_insertion_session(ours, theirs);
        let buffer = draw(&session, 180, 12);
        for (symbol, height) in [("»", ours_height), ("«", theirs_height)] {
            let (x, start) = cell_at(&buffer, symbol);
            for y in start..start + height {
                assert_eq!(
                    buffer[(x, y)].bg,
                    Color::Rgb(119, 136, 153),
                    "connector does not cover its source choice at ({x}, {y})",
                );
            }
            for y in [start - 1, start + height] {
                assert_eq!(buffer[(x, y)].bg, Color::Rgb(32, 32, 32));
                assert!(!matches!(buffer[(x, y)].symbol(), "▀" | "▄"));
            }
        }
    }
}

#[test]
fn an_empty_conflict_choice_does_not_shade_the_following_unchanged_line() {
    let session = ReviewSession::three_way(
        "head\nbase\ntail\n",
        "head\ntail\n",
        "head\nφreplacement\ntail\n",
    );
    assert_eq!(session.unresolved_conflicts(), 1);
    let buffer = draw(&session, 180, 12);
    let mut unchanged_tails = 0;
    for y in 2..6 {
        for x in 0..buffer.area.width {
            if buffer[(x, y)].symbol() == "t" && buffer[(x + 1, y)].symbol() == "a" {
                unchanged_tails += 1;
                assert_eq!(
                    buffer[(x + 12, y)].bg,
                    buffer[(x + 12, 2)].bg,
                    "unchanged tail is painted as a conflict at ({x}, {y})",
                );
            }
        }
    }
    assert_eq!(
        unchanged_tails, 3,
        "unchanged context must remain in all three panes"
    );
    let (x, y) = cell_at(&buffer, "φ");
    assert_ne!(buffer[(x + 12, y)].bg, buffer[(x + 12, y + 1)].bg);
}

#[test]
fn indexed_theme_preserves_terminal_colours_and_default_can_be_restored() {
    use ratatui::style::{Color, Modifier};
    let mut session = ReviewSession::two_way("let old = 1;\n", "let new = 2;\n");
    session.set_paths(Path::new("left.rs"), Path::new("right.rs"));
    let original = draw(&session, 100, 12);
    session.set_theme(external_theme(
        r#"
"ui.background" = { bg = "black" }
"ui.text" = { fg = "white" }
"keyword" = { fg = "magenta", modifiers = ["italic"] }
"diff.delta" = "blue"
"ui.selection" = { modifiers = ["underlined"] }
        "#,
    ));
    let themed = draw(&session, 100, 12);
    assert_eq!(themed[(5, 9)].bg, Color::Black);
    let keyword = cell_at(&themed, "e");
    assert_eq!(themed[keyword].fg, Color::Magenta);
    assert_eq!(themed[keyword].bg, Color::Black);
    assert!(
        themed[keyword]
            .modifier
            .contains(Modifier::ITALIC | Modifier::UNDERLINED)
    );
    assert!(
        !themed
            .content
            .iter()
            .any(|cell| matches!(cell.fg, Color::Rgb(..)) || matches!(cell.bg, Color::Rgb(..)))
    );
    session.set_theme(std::sync::Arc::new(chvrn_tui::Theme::default()));
    assert_eq!(draw(&session, 100, 12), original);
}

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

fn gutter_controls(buffer: &Buffer) -> String {
    buffer
        .content
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            let row = index / usize::from(buffer.area.width);
            row >= 2 && row + 1 < usize::from(buffer.area.height)
        })
        .map(|(_, cell)| cell.symbol())
        .filter(|symbol| matches!(*symbol, "»" | "«" | "↗" | "↘" | "↖" | "↙"))
        .collect()
}

#[test]
fn footer_keeps_shortcuts_ahead_of_the_focused_filename_and_distinguishes_keys() {
    let mut session = ReviewSession::two_way("old\n", "new\n");
    session.set_paths(
        Path::new("/workspace/booking-service/templates/cancellation/notice.ts"),
        Path::new("/workspace/booking-service/templates/cancellation/revised.ts"),
    );
    let buffer = draw(&session, 180, 12);
    let footer = row_text(&buffer, 11);
    let save = footer.find("[s]").expect("save shortcut must be visible");
    let filename = footer
        .find("notice.ts")
        .expect("focused filename must be visible");
    assert!(save < filename);
    assert!(footer.trim_end().ends_with("notice.ts"));
    assert!(!footer.contains("/workspace/"));
    assert_ne!(
        buffer[(save as u16 + 1, 11)].style(),
        buffer[(filename as u16, 11)].style()
    );
    assert_ne!(
        buffer[(save as u16 + 1, 11)].style(),
        buffer[(save as u16 + 4, 11)].style()
    );

    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Tab,
        KeyModifiers::NONE,
    )));
    let switched = row_text(&draw(&session, 180, 12), 11);
    assert!(switched.trim_end().ends_with("revised.ts"));
    assert!(!switched.contains("notice.ts"));
}

#[test]
fn footer_sacrifices_a_long_unicode_filename_before_clipping_essential_shortcuts() {
    let mut session = ReviewSession::two_way("old\n", "new\n");
    session.set_paths(
        Path::new("/workspace/通知/界界界界界界界界界界界界界界界界界界界界e\u{301}-cancellation-notice.ts"),
        Path::new("/workspace/revised.ts"),
    );

    for width in [64, 48, 32] {
        session.handle(ReviewInput::Resize { width, height: 10 });
        let footer = row_text(&draw(&session, width, 10), 9);
        for shortcut in ["[s]", "[q]", "[?]"] {
            assert!(
                footer.contains(shortcut),
                "{shortcut} disappeared at width {width}: {footer}"
            );
        }
        assert!(!footer.contains("/workspace/"));
    }
}

#[test]
fn merge_choice_hints_cannot_displace_save_quit_or_help_on_narrow_terminals() {
    let mut session = ReviewSession::three_way("base\n", "ours\n", "theirs\n");
    session.set_output_path(Path::new("/workspace/cancellation-notice.ts"));
    for width in [32, 48, 64, 80] {
        session.handle(ReviewInput::Resize { width, height: 10 });
        let footer = row_text(&draw(&session, width, 10), 9);
        for shortcut in ["[s]", "[q]", "[?]"] {
            assert!(
                footer.contains(shortcut),
                "{shortcut} disappeared at width {width}: {footer}"
            );
        }
    }
}

#[test]
fn shift_tab_reveals_the_previous_pane_in_a_narrow_merge() {
    let mut session = ReviewSession::three_way("BASEONLY\n", "OURSONLY\n", "THEIRSONLY\n");
    session.handle(ReviewInput::Resize {
        width: 26,
        height: 8,
    });
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::BackTab,
        KeyModifiers::SHIFT,
    )));
    let ours = visible_text(&draw(&session, 26, 8));
    assert_eq!(session.focus(), Pane::Ours);
    assert!(ours.contains("OURSONLY"));
    assert!(!ours.contains("Merged result"));
    assert!(!ours.contains("THEIRSONLY"));
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::BackTab,
        KeyModifiers::SHIFT,
    )));
    let theirs = visible_text(&draw(&session, 26, 8));
    assert_eq!(session.focus(), Pane::Theirs);
    assert!(theirs.contains("THEIRSONLY"));
    assert!(!theirs.contains("Merged result"));
    assert!(!theirs.contains("OURSONLY"));
}

#[test]
fn help_recovers_both_full_paths_when_the_footer_only_shows_a_filename() {
    let mut session = ReviewSession::two_way("old\n", "new\n");
    session.set_paths(
        Path::new("/workspace/first-component/src/notice.ts"),
        Path::new("/workspace/second-component/src/notice.ts"),
    );
    let footer = row_text(&draw(&session, 140, 20), 19);
    assert!(!footer.contains("/workspace/"));
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Char('?'),
        KeyModifiers::NONE,
    )));
    let help = visible_text(&draw(&session, 140, 20));
    assert!(help.contains("/workspace/first-component/src/notice.ts"));
    assert!(help.contains("/workspace/second-component/src/notice.ts"));
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Esc,
        KeyModifiers::NONE,
    )));
    let closed = visible_text(&draw(&session, 140, 20));
    assert!(!closed.contains("/workspace/first-component/"));
    assert!(!closed.contains("/workspace/second-component/"));
}

#[test]
fn footer_merge_choices_follow_remaining_conflicts_through_resolution_undo_and_redo() {
    let mut session = ReviewSession::three_way(
        "head\nbase-one\nmiddle\nbase-two\ntail\n",
        "head\nours-one\nmiddle\nours-two\ntail\n",
        "head\ntheirs-one\nmiddle\ntheirs-two\ntail\n",
    );
    assert_eq!(session.unresolved_conflicts(), 2);
    let initial = row_text(&draw(&session, 180, 12), 11);
    for shortcut in ["[o]", "[t]", "[b]"] {
        assert!(
            initial.contains(shortcut),
            "missing merge choice {shortcut}: {initial}"
        );
    }
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Char('o'),
        KeyModifiers::NONE,
    )));
    assert_eq!(session.unresolved_conflicts(), 1);
    let remaining = row_text(&draw(&session, 180, 12), 11);
    for shortcut in ["[o]", "[t]", "[b]"] {
        assert!(
            remaining.contains(shortcut),
            "remaining conflict lost {shortcut}: {remaining}"
        );
    }
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Char('t'),
        KeyModifiers::NONE,
    )));
    assert_eq!(session.unresolved_conflicts(), 0);
    let resolved = row_text(&draw(&session, 180, 12), 11);
    for shortcut in ["[o]", "[t]", "[b]"] {
        assert!(
            !resolved.contains(shortcut),
            "resolved merge still offers {shortcut}: {resolved}"
        );
    }
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Esc,
        KeyModifiers::NONE,
    )));
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Char('u'),
        KeyModifiers::NONE,
    )));
    assert_eq!(session.unresolved_conflicts(), 1);
    let undone = row_text(&draw(&session, 180, 12), 11);
    for shortcut in ["[o]", "[t]", "[b]"] {
        assert!(
            undone.contains(shortcut),
            "undo did not restore {shortcut}: {undone}"
        );
    }
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Char('r'),
        KeyModifiers::CONTROL,
    )));
    assert_eq!(session.unresolved_conflicts(), 0);
    let redone = row_text(&draw(&session, 180, 12), 11);
    for shortcut in ["[o]", "[t]", "[b]"] {
        assert!(
            !redone.contains(shortcut),
            "redo retained {shortcut}: {redone}"
        );
    }
}

#[test]
fn footer_insert_mode_offers_escape_instead_of_letter_commands_that_would_insert_text() {
    let mut session = ReviewSession::three_way("base\n", "ours\n", "theirs\n");
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Char('i'),
        KeyModifiers::NONE,
    )));
    let editing = row_text(&draw(&session, 180, 12), 11);
    assert!(editing.contains("[Esc]"));
    for shortcut in ["[o]", "[t]", "[b]", "[s]", "[q]", "[u]"] {
        assert!(
            !editing.contains(shortcut),
            "insert mode advertises review command {shortcut}"
        );
    }
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Char('s'),
        KeyModifiers::NONE,
    )));
    assert_eq!(session.pane_text(Pane::Result), "sours\n");
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Esc,
        KeyModifiers::NONE,
    )));
    let reviewing = row_text(&draw(&session, 180, 12), 11);
    assert!(reviewing.contains("[s]"));
    assert!(!reviewing.contains("[Esc]"));
}

#[test]
fn header_distinguishes_insert_mode_and_restores_review_styling_on_escape() {
    let mut session = ReviewSession::three_way("base\n", "ours\n", "theirs\n");
    let initial = draw(&session, 120, 10);
    let review_column = row_text(&initial, 0).find("REVIEW").unwrap() as u16;
    let review_style = initial[(review_column, 0)].style();
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Char('i'),
        KeyModifiers::NONE,
    )));
    assert!(session.is_editing());
    let editing = draw(&session, 120, 10);
    let insert_column = row_text(&editing, 0).find("INSERT").unwrap() as u16;
    let insert_style = editing[(insert_column, 0)].style();
    assert_ne!(insert_style, review_style);
    assert_ne!(
        editing[(insert_column, 0)].fg,
        editing[(insert_column, 0)].bg
    );
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Esc,
        KeyModifiers::NONE,
    )));
    assert!(!session.is_editing());
    let restored = draw(&session, 120, 10);
    let restored_column = row_text(&restored, 0).find("REVIEW").unwrap() as u16;
    assert_eq!(restored[(restored_column, 0)].style(), review_style);
}

#[test]
fn header_emphasises_modified_state_without_conflating_it_with_insert_mode() {
    let mut session = ReviewSession::three_way("base\n", "ours\n", "theirs\n");
    let initial = draw(&session, 120, 10);
    let clean_column = row_text(&initial, 0).find("clean").unwrap() as u16;
    let clean_style = initial[(clean_column, 0)].style();
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Char('i'),
        KeyModifiers::NONE,
    )));
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Char('X'),
        KeyModifiers::NONE,
    )));
    assert!(session.is_dirty());
    let changed = draw(&session, 120, 10);
    let header = row_text(&changed, 0);
    let modified_column = header.find("modified").unwrap() as u16;
    let insert_column = header.find("INSERT").unwrap() as u16;
    let modified_style = changed[(modified_column, 0)].style();
    assert_ne!(modified_style, clean_style);
    assert_ne!(modified_style, changed[(insert_column, 0)].style());
    assert_ne!(
        changed[(modified_column, 0)].fg,
        changed[(modified_column, 0)].bg
    );
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Esc,
        KeyModifiers::NONE,
    )));
    let reviewing = draw(&session, 120, 10);
    let modified_column = row_text(&reviewing, 0).find("modified").unwrap() as u16;
    assert_eq!(reviewing[(modified_column, 0)].style(), modified_style);
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
fn connector_edges_do_not_extend_unequal_replacements_into_unchanged_lines() {
    let short = "head\nOLD\ntail\n";
    let long = "head\nNEW1\nNEW2\nNEW3\nNEW4\nNEW5\ntail\n";
    for (left, right, left_rows, right_rows) in
        [(short, long, 3..4, 3..8), (long, short, 3..8, 3..4)]
    {
        let session = ReviewSession::two_way(left, right);
        let buffer = draw(&session, 80, 12);
        let (left_edge, _) = cell_at(&buffer, "»");
        let (right_edge, _) = cell_at(&buffer, "«");
        for (x, changed_rows) in [(left_edge, left_rows), (right_edge, right_rows)] {
            let rail = buffer[(x, 2)].bg;
            for y in 3..10 {
                if changed_rows.contains(&y) {
                    continue;
                }
                let cell = &buffer[(x, y)];
                assert_eq!(
                    cell.bg, rail,
                    "connector spills into unchanged row {y} at {x}"
                );
                assert!(
                    !matches!(cell.symbol(), "▀" | "▄"),
                    "connector half-block spills into unchanged row {y} at {x}",
                );
            }
        }
    }
}

#[test]
fn connector_edges_follow_changed_lines_after_an_earlier_insertion() {
    let session = ReviewSession::two_way(
        "head\nsame\nkeep\nOLD\ntail\n",
        "head\none\ntwo\nthree\nfour\nfive\nsame\nkeep\nNEW\ntail\n",
    );
    let buffer = draw(&session, 80, 15);
    let (left_edge, _) = cell_at(&buffer, "»");
    let (right_edge, _) = cell_at(&buffer, "«");
    for (x, y) in [(left_edge, 6), (right_edge, 9)] {
        let rail = buffer[(x, 2)].bg;
        let cell = &buffer[(x, y)];
        assert_eq!(
            cell.bg, rail,
            "connector reaches an unchanged line at ({x}, {y})"
        );
        assert!(
            !matches!(cell.symbol(), "▀" | "▄"),
            "connector half-block reaches an unchanged line at ({x}, {y})",
        );
    }
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
fn accepting_a_source_removes_its_gutter_controls_and_click_targets() {
    for (choice, accepted, expected) in [
        ('o', "»↗↘", "head\nours\ntail\n"),
        ('t', "«↖↙", "head\ntheirs\ntail\n"),
        ('b', "»«↗↘↖↙", "head\nours\ntheirs\ntail\n"),
    ] {
        let mut session = ReviewSession::three_way(
            "head\nbase\ntail\n",
            "head\nours\ntail\n",
            "head\ntheirs\ntail\n",
        );
        session.handle(ReviewInput::Resize {
            width: 120,
            height: 20,
        });
        let initial = draw(&session, 120, 20);
        let targets: Vec<_> = ["»", "«"]
            .into_iter()
            .filter(|symbol| accepted.contains(*symbol))
            .map(|symbol| (symbol, cell_at(&initial, symbol)))
            .collect();
        session.handle(ReviewInput::Key(KeyEvent::new(
            KeyCode::Char(choice),
            KeyModifiers::NONE,
        )));
        session.handle(ReviewInput::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )));
        let resolved = draw(&session, 120, 20);
        let controls = gutter_controls(&resolved);
        assert!(
            !controls.chars().any(|symbol| accepted.contains(symbol)),
            "accepted source still has controls: {controls}"
        );
        for (symbol, (x, y)) in targets {
            let inner_x = if symbol == "»" { x + 1 } else { x - 1 };
            for column in [x, inner_x] {
                session.handle(ReviewInput::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column,
                    row: y,
                    modifiers: KeyModifiers::NONE,
                }));
                assert_eq!(session.pane_text(Pane::Result), expected);
            }
        }
    }
}

#[test]
fn the_remaining_source_offers_insertion_below_without_an_above_control() {
    for (choice, below, expected) in [
        ('o', "↙", "head\nours\ntheirs\ntail\n"),
        ('t', "↘", "head\ntheirs\nours\ntail\n"),
    ] {
        let mut session = ReviewSession::three_way(
            "head\nbase\ntail\n",
            "head\nours\ntail\n",
            "head\ntheirs\ntail\n",
        );
        session.handle(ReviewInput::Resize {
            width: 120,
            height: 20,
        });
        session.handle(ReviewInput::Key(KeyEvent::new(
            KeyCode::Char(choice),
            KeyModifiers::NONE,
        )));
        session.handle(ReviewInput::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )));
        let buffer = draw(&session, 120, 20);
        let controls = gutter_controls(&buffer);
        assert!(
            !controls.contains(['↗', '↖']),
            "insert-above controls are still visible: {controls}"
        );
        let (column, row) = cell_at(&buffer, below);
        session.handle(ReviewInput::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(session.pane_text(Pane::Result), expected);
    }
}

#[test]
fn inserting_the_remaining_source_consumes_its_control_and_undo_restores_it() {
    let mut session = ReviewSession::three_way(
        "head\nbase\ntail\n",
        "head\nours\ntail\n",
        "head\ntheirs\ntail\n",
    );
    session.handle(ReviewInput::Resize {
        width: 120,
        height: 20,
    });
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Char('o'),
        KeyModifiers::NONE,
    )));
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Esc,
        KeyModifiers::NONE,
    )));
    let before = draw(&session, 120, 20);
    let (column, row) = cell_at(&before, "↙");
    session.handle(ReviewInput::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }));
    assert_eq!(
        session.pane_text(Pane::Result),
        "head\nours\ntheirs\ntail\n"
    );
    assert_eq!(gutter_controls(&draw(&session, 120, 20)), "");
    session.handle(ReviewInput::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }));
    assert_eq!(
        session.pane_text(Pane::Result),
        "head\nours\ntheirs\ntail\n"
    );
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Char('u'),
        KeyModifiers::NONE,
    )));
    assert_eq!(session.pane_text(Pane::Result), "head\nours\ntail\n");
    assert_eq!(gutter_controls(&draw(&session, 120, 20)), "↙");
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Char('r'),
        KeyModifiers::CONTROL,
    )));
    assert_eq!(
        session.pane_text(Pane::Result),
        "head\nours\ntheirs\ntail\n"
    );
    assert_eq!(gutter_controls(&draw(&session, 120, 20)), "");
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
fn overview_viewport_block_moves_with_the_visible_lines() {
    let text = (0..50)
        .map(|line| format!("line{line:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut session = ReviewSession::two_way(&text, &text);
    session.handle(ReviewInput::Resize {
        width: 80,
        height: 13,
    });
    let initial = draw(&session, 80, 13);
    for x in [36, 79] {
        let rows: Vec<_> = (2..12)
            .filter(|&y| initial[(x, y)].symbol() == "█")
            .collect();
        assert_eq!(rows, vec![2, 3]);
    }

    session.go_to(Pane::Left, 29, 0);
    let scrolled = draw(&session, 80, 13);
    for x in [36, 79] {
        let rows: Vec<_> = (2..12)
            .filter(|&y| scrolled[(x, y)].symbol() == "█")
            .collect();
        assert_eq!(rows, vec![6, 7]);
        assert_eq!(scrolled[(x, 2)].symbol(), "│");
    }
}

#[test]
fn overview_viewport_block_expands_when_more_of_the_file_becomes_visible() {
    let text = (0..50)
        .map(|line| format!("line{line:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut session = ReviewSession::two_way(&text, &text);
    session.handle(ReviewInput::Resize {
        width: 80,
        height: 13,
    });
    session.handle(ReviewInput::Resize {
        width: 80,
        height: 23,
    });
    let resized = draw(&session, 80, 23);
    for x in [36, 79] {
        let rows: Vec<_> = (2..22)
            .filter(|&y| resized[(x, y)].symbol() == "█")
            .collect();
        assert_eq!(rows, vec![2, 3, 4, 5, 6, 7, 8, 9]);
    }
}

#[test]
fn overview_viewport_block_is_continuous_when_the_whole_file_fits() {
    let mut session = ReviewSession::two_way("one\ntwo", "one\ntwo");
    session.handle(ReviewInput::Resize {
        width: 80,
        height: 13,
    });
    let buffer = draw(&session, 80, 13);
    for x in [36, 79] {
        for y in 2..12 {
            assert_eq!(
                buffer[(x, y)].symbol(),
                "█",
                "whole-file viewport has a gap at ({x}, {y})",
            );
        }
    }
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
