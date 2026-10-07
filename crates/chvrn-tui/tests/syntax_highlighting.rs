use chvrn_core::unified::UnifiedPatch;
use chvrn_tui::{PagerSession, Pane, ReviewInput, ReviewSession, Theme};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Terminal,
    backend::TestBackend,
    buffer::Buffer,
    style::{Color, Modifier},
};
use std::{path::Path, sync::Arc};

const KEYWORD: Color = Color::Rgb(145, 35, 69);
const STRING: Color = Color::Rgb(37, 147, 71);
const NUMBER: Color = Color::Rgb(39, 73, 149);

fn theme(keyword: &str, string: &str, number: &str) -> Arc<Theme> {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("syntax.toml");
    std::fs::write(
        &path,
        format!(
            r##"
"ui.background" = {{ bg = "#101010" }}
"ui.text" = {{ fg = "#cccccc" }}
"keyword" = {{ fg = "{keyword}", modifiers = ["italic"] }}
"string" = {{ fg = "{string}", modifiers = ["underlined"] }}
"constant.numeric" = {{ fg = "{number}", modifiers = ["bold"] }}
"ui.selection" = {{ bg = "#303030" }}
"ui.cursor" = {{ fg = "#ffffff", bg = "#606060" }}
"chvrn.diff.modified.old.selected.inline" = {{ bg = "#402020" }}
"chvrn.diff.modified.new.selected.inline" = {{ bg = "#204020" }}
"chvrn.diff.conflict.result.selected.inline" = {{ bg = "#404020" }}
"##
        ),
    )
    .unwrap();
    Arc::new(Theme::load(path.to_str().unwrap(), None).unwrap())
}

fn syntax_theme() -> Arc<Theme> {
    theme("#912345", "#259347", "#274995")
}

fn draw_review(session: &ReviewSession) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(210, 26)).unwrap();
    terminal.draw(|frame| session.render(frame)).unwrap();
    terminal.backend().buffer().clone()
}

fn draw_pager(session: &mut PagerSession) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(180, 26)).unwrap();
    terminal.draw(|frame| session.render(frame)).unwrap();
    terminal.backend().buffer().clone()
}

fn rows(buffer: &Buffer) -> Vec<String> {
    buffer
        .content
        .chunks(usize::from(buffer.area.width))
        .map(|row| row.iter().map(|cell| cell.symbol()).collect())
        .collect()
}

fn assert_token(
    buffer: &Buffer,
    source: &str,
    token: &str,
    occurrences: usize,
    color: Color,
    modifier: Modifier,
) {
    let offset = source.find(token).expect("token belongs to source fixture");
    let mut found = 0;
    for (y, row) in rows(buffer).iter().enumerate() {
        for (start, _) in row.match_indices(source) {
            let x = row[..start].chars().count() + source[..offset].chars().count();
            for column in x..x + token.chars().count() {
                let cell = &buffer[(column as u16, y as u16)];
                assert_eq!(cell.fg, color, "{token:?} in {source:?} at ({column}, {y})");
                assert!(
                    cell.modifier.contains(modifier),
                    "{token:?} in {source:?} lost {modifier:?} at ({column}, {y})"
                );
            }
            found += 1;
        }
    }
    assert_eq!(found, occurrences, "visible occurrences of {source:?}");
}

fn context_pager(path: &str, source: &str) -> PagerSession {
    let count = source.lines().count();
    let body: String = source.lines().map(|line| format!(" {line}\n")).collect();
    let patch = format!("--- a/{path}\n+++ b/{path}\n@@ -1,{count} +1,{count} @@\n{body}");
    PagerSession::new(UnifiedPatch::parse(patch).unwrap(), syntax_theme())
}

#[test]
fn plain_multibyte_gaps_do_not_inherit_neighbouring_syntax_styles() {
    let source = "let λ = \"text\"; let β = 42;";
    let mut session = ReviewSession::two_way(source, source);
    session.set_paths(Path::new("left.rs"), Path::new("right.rs"));
    session.set_theme(syntax_theme());
    let buffer = draw_review(&session);
    assert_token(&buffer, source, "text", 2, STRING, Modifier::UNDERLINED);
    assert_token(&buffer, source, "42", 2, NUMBER, Modifier::BOLD);
    let mut plain = 0;
    for cell in &buffer.content {
        if matches!(cell.symbol(), "λ" | "β") {
            plain += 1;
            assert_eq!(cell.fg, Color::Rgb(204, 204, 204));
            assert!(
                !cell
                    .modifier
                    .intersects(Modifier::ITALIC | Modifier::BOLD | Modifier::UNDERLINED)
            );
        }
    }
    assert_eq!(plain, 4);
}

#[test]
fn elixir_pager_styles_sparse_hunks_without_renumbering_source() {
    let patch = "--- a/lib/example.ex\n+++ b/lib/example.ex\n@@ -40,3 +70,3 @@\n   if true do\n-    label = \"before\"\n+    label = \"after\"\n   end\n@@ -900 +1200 @@\n-  count = 41\n+  count = 42\n";
    let mut pager = PagerSession::new(
        UnifiedPatch::parse(patch.to_owned()).unwrap(),
        syntax_theme(),
    );
    let buffer = draw_pager(&mut pager);
    let rendered = rows(&buffer);
    let keyword = rendered
        .iter()
        .find(|row| row.contains("if true do"))
        .unwrap();
    let (old, new) = keyword.split_once('│').unwrap();
    assert!(old.contains("40"));
    assert!(new.contains("70"));
    let number = rendered
        .iter()
        .find(|row| row.contains("count = 41"))
        .unwrap();
    let (old, new) = number.split_once('│').unwrap();
    assert!(old.contains("900"));
    assert!(new.contains("1200"));
    assert_token(&buffer, "  if true do", "if", 2, KEYWORD, Modifier::ITALIC);
    assert_token(
        &buffer,
        "    label = \"before\"",
        "before",
        1,
        STRING,
        Modifier::UNDERLINED,
    );
    assert_token(
        &buffer,
        "    label = \"after\"",
        "after",
        1,
        STRING,
        Modifier::UNDERLINED,
    );
    assert_token(&buffer, "  count = 41", "41", 1, NUMBER, Modifier::BOLD);
    assert_token(&buffer, "  count = 42", "42", 1, NUMBER, Modifier::BOLD);
}

#[test]
fn elixir_two_way_review_styles_both_sources_with_the_selected_theme() {
    let left = "  if true do\n    label = \"before\"\n    count = 41\n  end\n";
    let right = "  if true do\n    label = \"after\"\n    count = 42\n  end\n";
    let mut session = ReviewSession::two_way(left, right);
    session.set_paths(Path::new("before.ex"), Path::new("after.ex"));
    session.set_theme(syntax_theme());
    let buffer = draw_review(&session);
    assert_eq!(session.pane_text(Pane::Left), left);
    assert_eq!(session.pane_text(Pane::Right), right);
    assert_token(&buffer, "  if true do", "if", 2, KEYWORD, Modifier::ITALIC);
    assert_token(
        &buffer,
        "    label = \"before\"",
        "before",
        1,
        STRING,
        Modifier::UNDERLINED,
    );
    assert_token(
        &buffer,
        "    label = \"after\"",
        "after",
        1,
        STRING,
        Modifier::UNDERLINED,
    );
    assert_token(&buffer, "    count = 41", "41", 1, NUMBER, Modifier::BOLD);
    assert_token(&buffer, "    count = 42", "42", 1, NUMBER, Modifier::BOLD);
}

#[test]
fn elixir_three_way_merge_styles_ours_result_and_theirs() {
    let shared = "  if true do\n    label = \"hello\"\n    count = 42\n  end\n";
    let base = format!("{shared}  value = 1\n");
    let ours = format!("{shared}  value = 2\n");
    let theirs = format!("{shared}  value = 3\n");
    let mut session = ReviewSession::three_way(&base, &ours, &theirs);
    session.set_paths(Path::new("ours.ex"), Path::new("theirs.ex"));
    session.set_theme(syntax_theme());
    let result = session.pane_text(Pane::Result);
    let buffer = draw_review(&session);
    assert_eq!(session.pane_text(Pane::Ours), ours);
    assert_eq!(session.pane_text(Pane::Theirs), theirs);
    assert_eq!(session.pane_text(Pane::Result), result);
    assert_token(&buffer, "  if true do", "if", 3, KEYWORD, Modifier::ITALIC);
    assert_token(
        &buffer,
        "    label = \"hello\"",
        "hello",
        3,
        STRING,
        Modifier::UNDERLINED,
    );
    assert_token(&buffer, "    count = 42", "42", 3, NUMBER, Modifier::BOLD);
}

#[test]
fn dockerfile_filename_selects_syntax_without_an_extension() {
    let source = "FROM alpine:3.20\nRUN echo hello\n";
    let mut pager = context_pager("containers/Dockerfile", source);
    assert_token(
        &draw_pager(&mut pager),
        "FROM alpine:3.20",
        "FROM",
        2,
        KEYWORD,
        Modifier::ITALIC,
    );
    let mut session = ReviewSession::two_way(source, source);
    session.set_paths(Path::new("old/Dockerfile"), Path::new("new/Dockerfile"));
    session.set_theme(syntax_theme());
    assert_token(
        &draw_review(&session),
        "RUN echo hello",
        "RUN",
        2,
        KEYWORD,
        Modifier::ITALIC,
    );
}

#[test]
fn extensionless_sources_use_their_shebang_in_pager_and_review() {
    let source = "#!/usr/bin/env python3\nif True:\n    label = \"hello\"\n    count = 42\n";
    let mut pager = context_pager("bin/check", source);
    let mut session = ReviewSession::two_way(source, source);
    session.set_paths(Path::new("old/bin/check"), Path::new("new/bin/check"));
    session.set_theme(syntax_theme());
    for buffer in [draw_pager(&mut pager), draw_review(&session)] {
        assert_token(&buffer, "if True:", "if", 2, KEYWORD, Modifier::ITALIC);
        assert_token(
            &buffer,
            "    label = \"hello\"",
            "hello",
            2,
            STRING,
            Modifier::UNDERLINED,
        );
        assert_token(&buffer, "    count = 42", "42", 2, NUMBER, Modifier::BOLD);
    }
}

#[test]
fn existing_rust_and_tsx_syntax_keeps_configured_token_styles() {
    for (path, source, keyword) in [
        (
            "src/main.rs",
            "  let label = \"hello\"; let count = 42;",
            "let",
        ),
        (
            "src/view.tsx",
            "  const node = <span title=\"hello\">{42}</span>;",
            "const",
        ),
    ] {
        let text = format!("{source}\n");
        let mut pager = context_pager(path, &text);
        let mut session = ReviewSession::two_way(&text, &text);
        session.set_paths(Path::new(path), Path::new(path));
        session.set_theme(syntax_theme());
        for buffer in [draw_pager(&mut pager), draw_review(&session)] {
            assert_token(&buffer, source, keyword, 2, KEYWORD, Modifier::ITALIC);
            assert_token(&buffer, source, "hello", 2, STRING, Modifier::UNDERLINED);
            assert_token(&buffer, source, "42", 2, NUMBER, Modifier::BOLD);
        }
    }
}

#[test]
fn switching_elixir_theme_repaints_without_changing_source_cursor_or_selected_hunk() {
    let left = "  if true do\n    label = \"before\"\n    count = 41\n  end\n";
    let right = "  if true do\n    label = \"after\"\n    count = 42\n  end\n";
    let mut session = ReviewSession::two_way(left, right);
    session.set_paths(Path::new("before.ex"), Path::new("after.ex"));
    session.set_theme(syntax_theme());
    session.handle(ReviewInput::Key(KeyEvent::new(
        KeyCode::Down,
        KeyModifiers::NONE,
    )));
    let before = draw_review(&session);
    let cursor = session.cursor();
    let selected = session.selected_hunk();
    let ranges = session.selected_hunk_ranges();
    assert!(selected.is_some());
    assert_token(
        &before,
        "    label = \"after\"",
        "after",
        1,
        STRING,
        Modifier::UNDERLINED,
    );
    session.set_theme(theme("#a13557", "#37a159", "#395ba3"));
    let after = draw_review(&session);
    assert_eq!(rows(&after), rows(&before));
    assert_eq!(session.cursor(), cursor);
    assert_eq!(session.selected_hunk(), selected);
    assert_eq!(session.selected_hunk_ranges(), ranges);
    assert_eq!(session.pane_text(Pane::Left), left);
    assert_eq!(session.pane_text(Pane::Right), right);
    assert!(!session.is_dirty());
    assert_token(
        &after,
        "  if true do",
        "if",
        2,
        Color::Rgb(161, 53, 87),
        Modifier::ITALIC,
    );
    assert_token(
        &after,
        "    label = \"after\"",
        "after",
        1,
        Color::Rgb(55, 161, 89),
        Modifier::UNDERLINED,
    );
    assert_token(
        &after,
        "    count = 42",
        "42",
        1,
        Color::Rgb(57, 91, 163),
        Modifier::BOLD,
    );
}
