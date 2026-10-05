use chvrn_core::unified::UnifiedPatch;
use chvrn_tui::{PagerSession, Theme};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use std::sync::Arc;

fn session(source: &str) -> PagerSession {
    PagerSession::new(
        UnifiedPatch::parse(source.to_owned()).unwrap(),
        Arc::new(Theme::default()),
    )
}

fn draw(session: &mut PagerSession, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| session.render(frame)).unwrap();
    terminal.backend().buffer().clone()
}

fn rows(buffer: &Buffer) -> Vec<String> {
    buffer
        .content
        .chunks(usize::from(buffer.area.width))
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect()
}

fn screen(session: &mut PagerSession, width: u16, height: u16) -> String {
    rows(&draw(session, width, height)).join("\n")
}

fn key(session: &mut PagerSession, code: KeyCode, modifiers: KeyModifiers) -> bool {
    session.handle(Event::Key(KeyEvent::new(code, modifiers)))
}

const SPARSE: &str = "diff --git a/first.rs b/first.rs\n--- a/first.rs\n+++ b/first.rs\n@@ -40,2 +70,2 @@ first section\n anchor_first\n-old_first\n+new_first\n@@ -900,1 +1200,1 @@ distant section\n-old_distant\n+new_distant\ndiff --git a/second.rs b/second.rs\n--- a/second.rs\n+++ b/second.rs\n@@ -5 +8 @@ second section\n-old_second\n+new_second\n";

#[test]
fn sparse_hunks_keep_original_numbers_and_explicit_boundaries() {
    let mut pager = session(SPARSE);
    let buffer = draw(&mut pager, 160, 30);
    let rendered = rows(&buffer);
    let first = rendered
        .iter()
        .find(|line| line.contains("anchor_first"))
        .unwrap();
    let halves = first.split_once('│').unwrap();
    assert!(halves.0.contains("40"));
    assert!(halves.1.contains("70"));
    let distant = rendered
        .iter()
        .find(|line| line.contains("old_distant"))
        .unwrap();
    let halves = distant.split_once('│').unwrap();
    assert!(halves.0.contains("900"));
    assert!(halves.1.contains("1200"));
    assert!(
        rendered
            .iter()
            .any(|line| line.contains("@@ -40,2 +70,2 @@"))
    );
    assert!(
        rendered
            .iter()
            .any(|line| line.contains("@@ -900,1 +1200,1 @@"))
    );
    assert!(
        rendered
            .iter()
            .any(|line| line.to_lowercase().contains("omitted"))
    );
    assert!(!rendered.iter().any(|line| line.contains("old_second")));
}

#[test]
fn file_and_hunk_navigation_select_supplied_boundaries() {
    let mut pager = session(SPARSE);
    draw(&mut pager, 100, 10);
    assert!(!key(&mut pager, KeyCode::Char(']'), KeyModifiers::NONE));
    let next = screen(&mut pager, 100, 10);
    assert!(next.contains("old_distant"));
    assert!(!next.contains("old_first"));
    key(&mut pager, KeyCode::Char('['), KeyModifiers::NONE);
    assert!(screen(&mut pager, 100, 10).contains("old_first"));
    key(&mut pager, KeyCode::Char('n'), KeyModifiers::CONTROL);
    let next = screen(&mut pager, 100, 10);
    assert!(next.contains("second.rs"));
    assert!(next.contains("old_second"));
    assert!(!next.contains("old_distant"));
    key(&mut pager, KeyCode::Char('p'), KeyModifiers::CONTROL);
    assert!(screen(&mut pager, 100, 12).contains("old_first"));
}

#[test]
fn adjacent_hunks_never_pair_a_deletion_with_another_hunks_addition() {
    let mut pager =
        session("--- a/a.txt\n+++ b/a.txt\n@@ -3 +2,0 @@\n-only_old\n@@ -90,0 +90 @@\n+only_new\n");
    let buffer = draw(&mut pager, 100, 20);
    let rendered = rows(&buffer);
    let old = rendered
        .iter()
        .find(|row| row.contains("only_old"))
        .unwrap();
    assert!(!old.contains("only_new"));
    assert!(
        rendered
            .iter()
            .any(|row| row.contains("only_new") && !row.contains("only_old"))
    );
}

#[test]
fn narrow_focus_and_horizontal_scroll_preserve_tabs_and_wide_graphemes() {
    let old = format!("\t界e\u{301}{}OLD_END", "x".repeat(60));
    let new = format!("\t界e\u{301}{}NEW_END", "x".repeat(60));
    let mut pager = session(&format!(
        "--- a/wide.txt\n+++ b/wide.txt\n@@ -9 +12 @@\n-{old}\n+{new}\n"
    ));
    let initial = draw(&mut pager, 45, 12);
    let wide = initial
        .content
        .iter()
        .position(|cell| cell.symbol() == "界")
        .unwrap();
    assert_eq!(initial.content[wide + 2].symbol(), "e\u{301}");
    assert!(
        !initial
            .content
            .iter()
            .any(|cell| cell.symbol().contains('\t'))
    );
    for _ in 0..55 {
        key(&mut pager, KeyCode::Right, KeyModifiers::NONE);
    }
    let before = screen(&mut pager, 45, 12);
    assert!(before.contains("OLD_END") || before.contains("NEW_END"));
    key(&mut pager, KeyCode::Tab, KeyModifiers::NONE);
    let after = screen(&mut pager, 45, 12);
    assert_ne!(before.contains("OLD_END"), after.contains("OLD_END"));
    assert_ne!(before.contains("NEW_END"), after.contains("NEW_END"));
    assert!(!after.contains("\u{1b}"));
}

#[test]
fn metadata_only_and_binary_files_remain_reviewable() {
    let mut pager = session(
        "diff --git a/old.txt b/new.txt\nsimilarity index 100%\nrename from old.txt\nrename to new.txt\nold mode 100644\nnew mode 100755\ndiff --git a/image.png b/image.png\nindex 1234567..abcdef0 100644\nBinary files a/image.png and b/image.png differ\n",
    );
    let metadata = screen(&mut pager, 120, 20);
    for text in [
        "old.txt",
        "new.txt",
        "similarity index 100%",
        "100644",
        "100755",
    ] {
        assert!(metadata.contains(text), "missing {text}");
    }
    key(&mut pager, KeyCode::Char('n'), KeyModifiers::CONTROL);
    let binary = screen(&mut pager, 120, 20);
    assert!(binary.contains("image.png"));
    assert!(binary.to_lowercase().contains("binary"));
    assert!(!binary.contains("@@"));
}

#[test]
fn no_final_newline_and_control_characters_are_visible_without_terminal_controls() {
    let mut pager = session(
        "diff --git \"a/safe\\007\\033.txt\" \"b/safe\\007\\033.txt\"\n--- \"a/safe\\007\\033.txt\"\n+++ \"b/safe\\007\\033.txt\"\n@@ -1 +1 @@\n-old text\n\\ No newline at end of file\n+new text\n\\ No newline at end of file\n",
    );
    let visible = screen(&mut pager, 140, 20);
    assert!(visible.to_lowercase().contains("no newline"));
    assert!(visible.contains("old"));
    assert!(visible.contains("new"));
    assert!(!visible.chars().any(|ch| ch.is_control() && ch != '\n'));
}

#[test]
fn editing_paste_and_action_keys_cannot_change_the_patch() {
    let mut pager = session(SPARSE);
    let before = screen(&mut pager, 140, 30);
    for code in [
        KeyCode::Char('a'),
        KeyCode::Char('d'),
        KeyCode::Char('s'),
        KeyCode::Char('r'),
        KeyCode::Char('u'),
        KeyCode::Char('i'),
        KeyCode::Char('o'),
        KeyCode::Delete,
        KeyCode::Backspace,
        KeyCode::Enter,
    ] {
        assert!(!key(&mut pager, code, KeyModifiers::NONE));
    }
    for code in [
        KeyCode::Char('s'),
        KeyCode::Char('z'),
        KeyCode::Char('y'),
        KeyCode::Enter,
    ] {
        assert!(!key(&mut pager, code, KeyModifiers::CONTROL));
    }
    assert!(!pager.handle(Event::Paste("replacement".into())));
    assert_eq!(screen(&mut pager, 140, 30), before);
}

#[test]
fn help_closes_before_quit_and_does_not_offer_mutation_actions() {
    let mut pager = session(SPARSE);
    let before = screen(&mut pager, 100, 24);
    key(&mut pager, KeyCode::Char('?'), KeyModifiers::NONE);
    let help = screen(&mut pager, 100, 24);
    assert!(help.contains("Ctrl-N"));
    assert!(help.contains("Ctrl-P"));
    for forbidden in ["stage", "restore", "submit", "save", "copy into"] {
        assert!(!help.to_lowercase().contains(forbidden));
    }
    assert!(!key(&mut pager, KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(screen(&mut pager, 100, 24), before);
    key(&mut pager, KeyCode::Char('?'), KeyModifiers::NONE);
    assert!(!key(&mut pager, KeyCode::Char('q'), KeyModifiers::NONE));
    assert!(key(&mut pager, KeyCode::Char('q'), KeyModifiers::NONE));
}

#[test]
fn scrolling_reaches_last_lines_and_home_restores_start() {
    let body: String = (1..=60)
        .map(|number| format!(" line_{number:02}\n"))
        .collect();
    let mut pager = session(&format!(
        "--- a/long.txt\n+++ b/long.txt\n@@ -200,60 +300,60 @@\n{body}"
    ));
    let first = screen(&mut pager, 100, 10);
    assert!(first.contains("line_01"));
    key(&mut pager, KeyCode::PageDown, KeyModifiers::NONE);
    assert!(!screen(&mut pager, 100, 10).contains("line_01"));
    key(&mut pager, KeyCode::End, KeyModifiers::NONE);
    assert!(screen(&mut pager, 100, 10).contains("line_60"));
    key(&mut pager, KeyCode::Home, KeyModifiers::NONE);
    assert_eq!(screen(&mut pager, 100, 10), first);
    pager.handle(Event::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 10,
        row: 5,
        modifiers: KeyModifiers::NONE,
    }));
    assert_ne!(screen(&mut pager, 100, 10), first);
    for (code, modifiers) in [
        (KeyCode::Char('f'), KeyModifiers::CONTROL),
        (KeyCode::Char('d'), KeyModifiers::CONTROL),
        (KeyCode::Down, KeyModifiers::NONE),
    ] {
        key(&mut pager, KeyCode::Home, KeyModifiers::NONE);
        key(&mut pager, code, modifiers);
        assert_ne!(screen(&mut pager, 100, 10), first);
    }
}

#[test]
fn empty_patch_has_a_safe_exit_on_tiny_screens() {
    let mut pager = session("");
    assert!(
        screen(&mut pager, 60, 8)
            .to_lowercase()
            .contains("no changes")
    );
    draw(&mut pager, 1, 1);
    for code in [
        KeyCode::End,
        KeyCode::Home,
        KeyCode::Tab,
        KeyCode::Char(']'),
    ] {
        assert!(!key(&mut pager, code, KeyModifiers::NONE));
    }
    assert!(key(&mut pager, KeyCode::Esc, KeyModifiers::NONE));
}

#[test]
fn syntax_modifiers_and_intraline_emphasis_survive_cached_pager_rendering() {
    use ratatui::style::Modifier;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pager.toml");
    std::fs::write(&path, "\"ui.background\" = { bg = \"#ffffff\" }\n\"keyword\" = { modifiers = [\"italic\"] }\n\"diff.delta\" = \"#3040a0\"\n").unwrap();
    let theme = Arc::new(Theme::load(path.to_str().unwrap(), None).unwrap());
    let patch = UnifiedPatch::parse(
        "--- a/code.rs\n+++ b/code.rs\n@@ -20 +80 @@\n-let item = old;\n+let item = new;\n".into(),
    )
    .unwrap();
    let mut pager = PagerSession::new(patch, theme);
    let buffer = draw(&mut pager, 100, 14);
    let source = buffer
        .content
        .chunks(100)
        .find(|row| {
            row.iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
                .contains("let item = old;")
        })
        .unwrap();
    let keyword = source.iter().find(|cell| cell.symbol() == "l").unwrap();
    let changed = source.iter().find(|cell| cell.symbol() == "o").unwrap();
    assert!(keyword.modifier.contains(Modifier::ITALIC));
    assert!(changed.modifier.contains(Modifier::BOLD));
    assert_eq!(draw(&mut pager, 100, 14), buffer);
}

#[test]
fn no_color_keeps_patch_markers_without_emitting_colours() {
    if std::env::var_os("CHVRN_PAGER_NO_COLOR_TEST").is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "no_color_keeps_patch_markers_without_emitting_colours",
            ])
            .env("CHVRN_PAGER_NO_COLOR_TEST", "1")
            .env("NO_COLOR", "1")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    let mut pager = session(SPARSE);
    let buffer = draw(&mut pager, 140, 30);
    let visible = rows(&buffer).join("\n");
    assert!(visible.contains("old_first"));
    assert!(visible.contains("new_first"));
    let source = rows(&buffer)
        .into_iter()
        .find(|row| row.contains("old_first"))
        .unwrap();
    assert!(source.contains("- "));
    assert!(source.contains("+ "));
    assert!(
        buffer
            .content
            .iter()
            .all(|cell| cell.fg == ratatui::style::Color::Reset
                && cell.bg == ratatui::style::Color::Reset)
    );
}
