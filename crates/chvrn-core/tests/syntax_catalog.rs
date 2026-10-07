use chvrn_core::TextSnapshot;
use chvrn_core::syntax::{HighlightKind, HighlightSpan, SyntaxCatalog};
use std::ops::Range;
use std::path::Path;

const CUSTOM_SYNTAX: &str = r#"%YAML 1.2
---
name: Deployment Rules
file_extensions: [chvrn-rules]
scope: source.chvrn-rules
contexts:
  main:
    - match: '\b(deploy|rollback)\b'
      scope: keyword.control.chvrn-rules
    - match: '"'
      push:
        - meta_scope: string.quoted.double.chvrn-rules
        - match: '"'
          pop: true
"#;

fn snapshot(source: &str) -> TextSnapshot {
    TextSnapshot::from_bytes(source.as_bytes()).unwrap()
}

fn covers(
    spans: &[HighlightSpan],
    mut bytes: Range<usize>,
    expected: impl Fn(&HighlightKind) -> bool,
) -> bool {
    bytes.all(|byte| {
        spans
            .iter()
            .any(|span| span.bytes.contains(&byte) && expected(&span.kind))
    })
}

#[test]
fn bundled_syntaxes_cover_programming_configuration_stylesheets_and_queries() {
    let catalogue = SyntaxCatalog::bundled();
    for (path, text) in [
        ("main.go", "package main\nvar message = \"hello\"\n"),
        ("main.rb", "puts \"hello\"\n"),
        ("Main.java", "class Main { String message = \"hello\"; }\n"),
        ("config.yaml", "message: \"hello\"\n"),
        ("main.tf", "variable \"message\" { default = \"hello\" }\n"),
        ("style.css", "a::before { content: \"hello\"; }\n"),
        ("query.sql", "SELECT 'hello';\n"),
    ] {
        let source = snapshot(text);
        let spans = catalogue.highlight(Path::new(path), &source).unwrap();
        let start = text.find("hello").unwrap();
        assert!(
            covers(&spans, start..start + 5, |kind| matches!(
                kind,
                HighlightKind::String
            )),
            "string token was not highlighted in {path}"
        );
    }
}

#[test]
fn custom_syntax_colours_a_previously_unknown_file_type() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("deployment.sublime-syntax"),
        CUSTOM_SYNTAX,
    )
    .unwrap();
    let source = snapshot("deploy \"blue\"\n");
    let path = Path::new("release.chvrn-rules");
    let plain = SyntaxCatalog::bundled().highlight(path, &source).unwrap();
    assert!(plain.is_empty());

    let catalogue = SyntaxCatalog::with_custom_syntaxes(directory.path()).unwrap();
    let spans = catalogue.highlight(path, &source).unwrap();
    assert!(covers(&spans, 0..6, |kind| matches!(
        kind,
        HighlightKind::Keyword
    )));
    assert!(covers(&spans, 8..12, |kind| matches!(
        kind,
        HighlightKind::String
    )));
}

#[test]
fn custom_extension_overrides_do_not_remove_other_bundled_languages() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("override.sublime-syntax"),
        CUSTOM_SYNTAX.replace("file_extensions: [chvrn-rules]", "file_extensions: [rs]"),
    )
    .unwrap();
    let catalogue = SyntaxCatalog::with_custom_syntaxes(directory.path()).unwrap();
    let overridden = catalogue
        .highlight(Path::new("release.rs"), &snapshot("deploy \"blue\"\n"))
        .unwrap();
    assert!(covers(&overridden, 0..6, |kind| matches!(
        kind,
        HighlightKind::Keyword
    )));

    let python = catalogue
        .highlight(Path::new("release.py"), &snapshot("return 42\n"))
        .unwrap();
    assert!(covers(&python, 0..6, |kind| matches!(
        kind,
        HighlightKind::Keyword
    )));
    assert!(covers(&python, 7..9, |kind| matches!(
        kind,
        HighlightKind::Number
    )));
}

#[test]
fn invalid_custom_syntax_reports_the_definition_that_cannot_be_loaded() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("broken.sublime-syntax");
    std::fs::write(&path, "name: Broken\ncontexts: [\n").unwrap();
    let error = match SyntaxCatalog::with_custom_syntaxes(directory.path()) {
        Ok(_) => panic!("an invalid custom definition must not silently become plain text"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("broken.sublime-syntax"));
}

#[test]
fn unicode_before_a_token_does_not_shift_its_highlight_range() {
    let catalogue = SyntaxCatalog::bundled();
    let source = snapshot("label = \"界e\u{301}\"; count = 42\n");
    let spans = catalogue
        .highlight(Path::new("release.exs"), &source)
        .unwrap();
    let number = source.text().find("42").unwrap();
    assert!(covers(&spans, number..number + 2, |kind| matches!(
        kind,
        HighlightKind::Number
    )));
    for span in &spans {
        assert!(source.text().is_char_boundary(span.bytes.start));
        assert!(source.text().is_char_boundary(span.bytes.end));
        assert!(span.bytes.start <= span.bytes.end);
        assert!(span.bytes.end <= source.as_bytes().len());
    }
}

#[test]
fn multiline_string_state_is_kept_within_one_source_snapshot() {
    let catalogue = SyntaxCatalog::bundled();
    let source = snapshot("message = \"\"\"\ndeploy 42\n\"\"\"\ncount = 42\n");
    let spans = catalogue
        .highlight(Path::new("release.ex"), &source)
        .unwrap();
    let inside = source.text().find("deploy 42").unwrap();
    let outside = source.text().rfind("42").unwrap();
    assert!(covers(&spans, inside..inside + 9, |kind| matches!(
        kind,
        HighlightKind::String
    )));
    assert!(covers(&spans, outside..outside + 2, |kind| matches!(
        kind,
        HighlightKind::Number
    )));
}

#[test]
fn an_unclosed_string_cannot_leak_into_the_next_independent_snapshot() {
    let catalogue = SyntaxCatalog::bundled();
    let first = catalogue
        .highlight(
            Path::new("release.ex"),
            &snapshot("message = \"\"\"\ninside\n"),
        )
        .unwrap();
    assert!(covers(&first, 14..20, |kind| matches!(
        kind,
        HighlightKind::String
    )));

    let second = catalogue
        .highlight(Path::new("release.ex"), &snapshot("count = 42\n"))
        .unwrap();
    assert!(covers(&second, 8..10, |kind| matches!(
        kind,
        HighlightKind::Number
    )));
    assert!(!covers(&second, 8..10, |kind| matches!(
        kind,
        HighlightKind::String
    )));
}

#[test]
fn an_unknown_extension_does_not_guess_a_language_from_code_like_text() {
    let catalogue = SyntaxCatalog::bundled();
    let source = snapshot("fn release() { return 42; }\n");
    let spans = catalogue
        .highlight(Path::new("notes.chvrn-unknown-syntax"), &source)
        .unwrap();
    assert!(spans.is_empty());
}

#[test]
fn filename_detection_and_shebang_detection_do_not_need_a_file_on_disk() {
    let catalogue = SyntaxCatalog::bundled();
    let dockerfile = snapshot("FROM alpine\nRUN echo \"hello\"\n");
    let spans = catalogue
        .highlight(Path::new("Dockerfile"), &dockerfile)
        .unwrap();
    assert!(covers(&spans, 0..4, |kind| *kind == HighlightKind::Keyword));

    let script = snapshot("#!/usr/bin/env python3\nreturn 42\n");
    for path in ["release", "release.chvrn-unknown-syntax"] {
        let spans = catalogue.highlight(Path::new(path), &script).unwrap();
        let start = script.text().find("return").unwrap();
        assert!(covers(&spans, start..start + 6, |kind| *kind
            == HighlightKind::Keyword));
        assert!(covers(&spans, start + 7..start + 9, |kind| *kind
            == HighlightKind::Number));
    }
}

#[test]
fn an_extension_takes_precedence_over_a_shebang() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("deployment.sublime-syntax"),
        CUSTOM_SYNTAX,
    )
    .unwrap();
    let catalogue = SyntaxCatalog::with_custom_syntaxes(directory.path()).unwrap();
    let source = snapshot("#!/usr/bin/env python3\ndeploy \"blue\"\n");
    let spans = catalogue
        .highlight(Path::new("release.chvrn-rules"), &source)
        .unwrap();
    let start = source.text().find("deploy").unwrap();
    assert!(covers(&spans, start..start + 6, |kind| *kind
        == HighlightKind::Keyword));
}

#[test]
fn original_rust_and_json_tokens_retain_their_categories_and_byte_ranges() {
    let catalogue = SyntaxCatalog::bundled();
    let rust = snapshot("fn value() -> i32 { 42 }\n");
    let rust_spans = catalogue.highlight(Path::new("value.rs"), &rust).unwrap();
    assert!(covers(&rust_spans, 3..8, |kind| *kind == HighlightKind::Function));
    assert!(covers(&rust_spans, 20..22, |kind| *kind == HighlightKind::Number));

    let json = snapshot("{\"value\": 2}");
    let json_spans = catalogue.highlight(Path::new("value.json"), &json).unwrap();
    assert!(covers(&json_spans, 1..8, |kind| *kind == HighlightKind::String));
    assert!(covers(&json_spans, 10..11, |kind| *kind == HighlightKind::Number));
}

#[test]
fn language_declaration_keywords_are_not_plain_identifiers() {
    let catalogue = SyntaxCatalog::bundled();
    for (path, source, bytes) in [
        ("value.rs", "fn value() {}\n", 0..2),
        ("value.ts", "const value = 1;\n", 0..5),
        ("value.py", "def value():\n    return 1\n", 0..3),
        (
            "value.tsx",
            "const View = () => <div title=\"hello\">{42}</div>;\n",
            0..5,
        ),
    ] {
        let spans = catalogue
            .highlight(Path::new(path), &snapshot(source))
            .unwrap();
        assert!(
            covers(&spans, bytes, |kind| *kind == HighlightKind::Keyword),
            "{path}"
        );
    }
}

#[test]
fn tsx_keeps_string_number_and_comment_categories() {
    let catalogue = SyntaxCatalog::bundled();
    let source = snapshot("const View = () => <div title=\"hello\">{42}</div>; // note\n");
    let spans = catalogue.highlight(Path::new("view.tsx"), &source).unwrap();
    for (token, expected) in [
        ("hello", HighlightKind::String),
        ("42", HighlightKind::Number),
        ("note", HighlightKind::Comment),
    ] {
        let start = source.text().find(token).unwrap();
        assert!(covers(&spans, start..start + token.len(), |kind| *kind == expected));
    }
}

#[test]
fn fragments_use_only_the_actual_first_source_line_for_shebang_detection() {
    let catalogue = SyntaxCatalog::bundled();
    let path = Path::new("release");
    let source = snapshot("return 42\n");
    let spans = catalogue
        .highlight_fragment(path, Some("#!/usr/bin/env python3"), &source)
        .unwrap();
    assert!(covers(&spans, 0..6, |kind| *kind == HighlightKind::Keyword));
    assert!(covers(&spans, 7..9, |kind| *kind == HighlightKind::Number));

    let later_shebang = snapshot("#!/usr/bin/env python3\nreturn 42\n");
    for first_line in [None, Some("ordinary text")] {
        let spans = catalogue
            .highlight_fragment(path, first_line, &later_shebang)
            .unwrap();
        assert!(spans.is_empty());
    }
}
