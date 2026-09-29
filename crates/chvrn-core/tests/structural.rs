use std::path::Path;

use chvrn_core::TextSnapshot;
use chvrn_core::structural::{
    HighlightKind, Language, StructuralAnalysis, StructuralChangeKind, StructuralError, highlight,
};

fn snapshot(text: &str) -> TextSnapshot {
    TextSnapshot::from_bytes(text.as_bytes()).unwrap()
}

#[test]
fn moving_an_unchanged_rust_function_reports_its_original_and_new_source_ranges() {
    let before = snapshot("fn moved() {}\nfn first() {}\nfn last() {}\n");
    let after = snapshot("fn first() {}\nfn last() {}\nfn moved() {}\n");
    let analysis = StructuralAnalysis::compare(Some(Language::Rust), &before, &after).unwrap();

    assert_eq!(analysis.changes().len(), 1);
    let change = &analysis.changes()[0];
    assert_eq!(change.kind, StructuralChangeKind::Move);
    assert_eq!(
        std::str::from_utf8(&before.as_bytes()[change.before.clone()])
            .unwrap()
            .trim(),
        "fn moved() {}"
    );
    assert_eq!(
        std::str::from_utf8(&after.as_bytes()[change.after.clone()])
            .unwrap()
            .trim(),
        "fn moved() {}"
    );
}

#[test]
fn changing_only_rust_layout_reports_reflow_instead_of_changed_tokens() {
    let before = snapshot("fn total(a:i32,b:i32)->i32{a+b}\n");
    let after = snapshot("fn total(a: i32, b: i32) -> i32 {\n    a + b\n}\n");
    let analysis = StructuralAnalysis::compare(Some(Language::Rust), &before, &after).unwrap();

    assert_eq!(analysis.changes().len(), 1);
    let change = &analysis.changes()[0];
    assert_eq!(change.kind, StructuralChangeKind::Reflow);
    assert!(
        std::str::from_utf8(&before.as_bytes()[change.before.clone()])
            .unwrap()
            .contains("a+b")
    );
    assert!(
        std::str::from_utf8(&after.as_bytes()[change.after.clone()])
            .unwrap()
            .contains("a + b")
    );
}

#[test]
fn changed_rust_literal_is_neither_a_move_nor_a_reflow() {
    let before = snapshot("fn count() -> i32 { 1 }\n");
    let after = snapshot("fn count() -> i32 { 2 }\n");
    let analysis = StructuralAnalysis::compare(Some(Language::Rust), &before, &after).unwrap();

    assert_eq!(analysis.changes().len(), 1);
    let change = &analysis.changes()[0];
    assert_eq!(change.kind, StructuralChangeKind::ChangedTokens);
    assert!(
        std::str::from_utf8(&before.as_bytes()[change.before.clone()])
            .unwrap()
            .contains("1")
    );
    assert!(
        std::str::from_utf8(&after.as_bytes()[change.after.clone()])
            .unwrap()
            .contains("2")
    );
}

#[test]
fn changing_only_a_rust_declaration_name_reports_a_rename_not_a_token_edit() {
    let before = snapshot("fn total() -> i32 { 1 }\n");
    let after = snapshot("fn sum() -> i32 { 1 }\n");
    let analysis = StructuralAnalysis::compare(Some(Language::Rust), &before, &after).unwrap();

    assert_eq!(analysis.changes().len(), 1);
    let change = &analysis.changes()[0];
    assert_eq!(change.kind, StructuralChangeKind::Renamed);
    assert_eq!(
        std::str::from_utf8(&before.as_bytes()[change.before.clone()])
            .unwrap()
            .trim(),
        "fn total() -> i32 { 1 }"
    );
    assert_eq!(
        std::str::from_utf8(&after.as_bytes()[change.after.clone()])
            .unwrap()
            .trim(),
        "fn sum() -> i32 { 1 }"
    );
}

#[test]
fn changing_a_javascript_call_target_is_a_token_edit_not_a_declaration_rename() {
    let before = snapshot("foo();\n");
    let after = snapshot("bar();\n");
    let analysis =
        StructuralAnalysis::compare(Some(Language::JavaScript), &before, &after).unwrap();

    assert_eq!(analysis.changes().len(), 1);
    let change = &analysis.changes()[0];
    assert_eq!(change.kind, StructuralChangeKind::ChangedTokens);
    assert_eq!(&before.as_bytes()[change.before.clone()], b"foo();");
    assert_eq!(&after.as_bytes()[change.after.clone()], b"bar();");
}

#[test]
fn each_registered_tree_sitter_grammar_finds_a_changed_token_in_its_own_syntax() {
    let cases = [
        (
            "a.rs",
            "fn value() -> i32 { 1 }\n",
            "fn value() -> i32 { 2 }\n",
        ),
        (
            "a.ts",
            "const value: number = 1;\n",
            "const value: number = 2;\n",
        ),
        (
            "a.tsx",
            "const View = () => <div>1</div>;\n",
            "const View = () => <div>2</div>;\n",
        ),
        (
            "a.js",
            "function value() { return 1; }\n",
            "function value() { return 2; }\n",
        ),
        (
            "a.jsx",
            "const View = () => <div>1</div>;\n",
            "const View = () => <div>2</div>;\n",
        ),
        (
            "a.py",
            "def value():\n    return 1\n",
            "def value():\n    return 2\n",
        ),
        ("a.json", "{\"value\": 1}\n", "{\"value\": 2}\n"),
    ];

    for (path, before_text, after_text) in cases {
        let language = Language::for_path(Path::new(path)).unwrap();
        let before = snapshot(before_text);
        let after = snapshot(after_text);
        let analysis = StructuralAnalysis::compare(Some(language), &before, &after).unwrap();

        assert_eq!(analysis.changes().len(), 1, "{path}");
        let change = &analysis.changes()[0];
        assert_eq!(change.kind, StructuralChangeKind::ChangedTokens, "{path}");
        assert!(
            std::str::from_utf8(&before.as_bytes()[change.before.clone()])
                .unwrap()
                .contains('1'),
            "{path}"
        );
        assert!(
            std::str::from_utf8(&after.as_bytes()[change.after.clone()])
                .unwrap()
                .contains('2'),
            "{path}"
        );
    }
}

#[test]
fn unregistered_text_retains_textual_diff_but_has_no_structural_parser() {
    let before = snapshot("before\n");
    let after = snapshot("after\n");
    let language = Language::for_path(Path::new("notes.md"));

    assert_eq!(language, None);
    assert!(matches!(
        StructuralAnalysis::compare(language, &before, &after),
        Err(StructuralError::UnsupportedLanguage)
    ));
    assert_eq!(before.as_bytes(), b"before\n");
    assert_eq!(after.as_bytes(), b"after\n");
}

#[test]
fn grammar_highlights_identifiers_and_literals_at_original_byte_ranges() {
    let rust = snapshot("fn value() -> i32 { 42 }\n");
    let rust_spans = highlight(Some(Language::Rust), &rust).unwrap();
    assert!(
        rust_spans
            .iter()
            .any(|span| span.bytes == (3..8) && span.kind == HighlightKind::Function)
    );
    assert!(
        rust_spans
            .iter()
            .any(|span| span.bytes == (20..22) && span.kind == HighlightKind::Number)
    );

    let json = snapshot("{\"value\": 2}");
    let json_spans = highlight(Some(Language::Json), &json).unwrap();
    assert!(
        json_spans
            .iter()
            .any(|span| span.bytes == (1..8) && span.kind == HighlightKind::String)
    );
    assert!(
        json_spans
            .iter()
            .any(|span| span.bytes == (10..11) && span.kind == HighlightKind::Number)
    );
}

#[test]
fn grammar_queries_highlight_language_keywords_not_just_identifiers() {
    for (language, source, bytes) in [
        (Language::Rust, "fn value() {}\n", 0..2),
        (Language::TypeScript, "const value = 1;\n", 0..5),
        (Language::Python, "def value():\n    return 1\n", 0..3),
    ] {
        let snapshot = snapshot(source);
        let spans = highlight(Some(language), &snapshot).unwrap();
        assert!(
            spans
                .iter()
                .any(|span| span.bytes == bytes && span.kind == HighlightKind::Keyword)
        );
    }
}
