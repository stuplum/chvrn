use chvrn_core::unified::{PatchLineKind, UnifiedPatch};

#[test]
fn git_changes_preserve_paths_hunk_boundaries_and_original_numbers() {
    let patch = UnifiedPatch::parse(
        concat!(
            "diff --git a/src/main.rs b/src/main.rs\n",
            "index 1111111..2222222 100644\n",
            "--- a/src/main.rs\n+++ b/src/main.rs\n",
            "@@ -3,3 +3,3 @@ fn main() {\n same\n-old\n+new\n end\n",
            "@@ -200 +200,2 @@ fn later() {\n keep\n+added\n",
            "diff --git a/new.txt b/new.txt\nnew file mode 100644\n",
            "--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+hello\n"
        )
        .to_owned(),
    )
    .unwrap();
    assert_eq!(patch.files.len(), 2);
    let file = &patch.files[0];
    assert_eq!(file.old_path.as_deref(), Some("src/main.rs"));
    assert_eq!(file.new_path.as_deref(), Some("src/main.rs"));
    assert!(
        file.metadata
            .iter()
            .any(|line| line == "index 1111111..2222222 100644")
    );
    assert_eq!(file.hunks.len(), 2);
    assert_eq!(file.hunks[0].heading, "fn main() {");
    let rows: Vec<_> = file.hunks[0]
        .lines
        .iter()
        .map(|line| {
            (
                line.kind,
                patch.text(&line.text),
                line.old_number,
                line.new_number,
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            (PatchLineKind::Context, "same", Some(3), Some(3)),
            (PatchLineKind::Removed, "old", Some(4), None),
            (PatchLineKind::Added, "new", None, Some(4)),
            (PatchLineKind::Context, "end", Some(5), Some(5)),
        ]
    );
    assert_eq!(file.hunks[1].lines[0].old_number, Some(200));
    assert_eq!(file.hunks[1].lines[1].new_number, Some(201));
    assert_eq!(patch.files[1].old_path, None);
    assert_eq!(patch.files[1].new_path.as_deref(), Some("new.txt"));
    assert_eq!(patch.files[1].hunks[0].old_count, 0);
}

#[test]
fn ordinary_unified_diffs_keep_spaces_unicode_and_timestamp_headers() {
    let patch = UnifiedPatch::parse(
        concat!(
            "--- old file λ.txt\t2026-10-01 12:00:00 +0000\n",
            "+++ new file λ.txt\t2026-10-02 12:00:00 +0000\n",
            "@@ -1 +1 @@\n-before\n+after\n",
            "--- gone.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-removed\n"
        )
        .to_owned(),
    )
    .unwrap();
    assert_eq!(patch.files[0].old_path.as_deref(), Some("old file λ.txt"));
    assert_eq!(patch.files[0].new_path.as_deref(), Some("new file λ.txt"));
    assert_eq!(patch.files[1].old_path.as_deref(), Some("gone.txt"));
    assert_eq!(patch.files[1].new_path, None);
}

#[test]
fn header_like_lines_remain_body_until_declared_counts_are_consumed() {
    let patch = UnifiedPatch::parse(
        concat!(
            "--- a/x\n+++ b/x\n@@ -1,3 +1,3 @@\n",
            "--- looks like a header\n+++ looks like a header\n",
            " diff --git a/not-a-file b/not-a-file\n @@ -10 +10 @@\n"
        )
        .to_owned(),
    )
    .unwrap();
    assert_eq!(patch.files.len(), 1);
    let lines = &patch.files[0].hunks[0].lines;
    assert_eq!(lines.len(), 4);
    assert_eq!(patch.text(&lines[0].text), "-- looks like a header");
    assert_eq!(patch.text(&lines[1].text), "++ looks like a header");
    assert_eq!(
        patch.text(&lines[2].text),
        "diff --git a/not-a-file b/not-a-file"
    );
}

#[test]
fn mode_rename_copy_and_empty_file_changes_survive_without_hunks() {
    let patch = UnifiedPatch::parse(concat!(
        "diff --git a/run b/run\nold mode 100644\nnew mode 100755\n",
        "diff --git a/old name b/new name\nsimilarity index 100%\nrename from old name\nrename to new name\n",
        "diff --git a/original b/copy\nsimilarity index 100%\ncopy from original\ncopy to copy\n",
        "diff --git a/empty b/empty\nnew file mode 100644\nindex 0000000..e69de29\n",
        "diff --git a/gone b/gone\ndeleted file mode 100644\nindex e69de29..0000000\n"
    ).to_owned()).unwrap();
    assert_eq!(patch.files.len(), 5);
    assert!(patch.files.iter().all(|file| file.hunks.is_empty()));
    assert_eq!(
        patch.files[0].metadata,
        ["old mode 100644", "new mode 100755"]
    );
    assert_eq!(patch.files[1].old_path.as_deref(), Some("old name"));
    assert_eq!(patch.files[1].new_path.as_deref(), Some("new name"));
    assert_eq!(patch.files[2].new_path.as_deref(), Some("copy"));
    assert_eq!(patch.files[3].old_path, None);
    assert_eq!(patch.files[4].new_path, None);
}

#[test]
fn binary_summaries_and_git_binary_payloads_do_not_become_source_lines() {
    let patch = UnifiedPatch::parse(concat!(
        "diff --git a/logo.png b/logo.png\nindex 1234567..abcdef0 100644\n",
        "Binary files a/logo.png and b/logo.png differ\n",
        "diff --git a/empty.bin b/empty.bin\nnew file mode 100644\n",
        "index 0000000..1234567\nGIT binary patch\nliteral 3\nKcmZQzU|?Vb00001\n\nliteral 0\nHcmV?d00001\n\n",
        "diff --git a/text b/text\n--- a/text\n+++ b/text\n@@ -1 +1 @@\n-a\n+b\n"
    ).to_owned()).unwrap();
    assert_eq!(patch.files.len(), 3);
    assert!(patch.files[0].binary);
    assert!(patch.files[1].binary);
    assert!(patch.files[0].hunks.is_empty());
    assert!(patch.files[1].hunks.is_empty());
    assert!(
        patch.files[1]
            .metadata
            .iter()
            .any(|line| line == "GIT binary patch")
    );
    assert!(
        !patch.files[1]
            .metadata
            .iter()
            .any(|line| line.starts_with("Kcm"))
    );
    assert_eq!(patch.text(&patch.files[2].hunks[0].lines[1].text), "b");
}

#[test]
fn quoted_git_paths_decode_octal_utf8_quotes_backslashes_and_newlines() {
    let patch = UnifiedPatch::parse(concat!(
        "diff --git \"a/caf\\303\\251\\t\\\"\\\\\\n.rs\" \"b/caf\\303\\251\\t\\\"\\\\\\n.rs\"\n",
        "--- \"a/caf\\303\\251\\t\\\"\\\\\\n.rs\"\n",
        "+++ \"b/caf\\303\\251\\t\\\"\\\\\\n.rs\"\n",
        "@@ -1 +1 @@\n-a\n+b\n"
    ).to_owned()).unwrap();
    assert_eq!(patch.files[0].old_path.as_deref(), Some("café\t\"\\\n.rs"));
    assert_eq!(patch.files[0].new_path.as_deref(), Some("café\t\"\\\n.rs"));
}

#[test]
fn four_byte_unicode_git_path_escapes_do_not_overflow() {
    let patch = UnifiedPatch::parse(
        "diff --git \"a/\\360\\237\\230\\200\" \"b/\\360\\237\\230\\200\"\nold mode 100644\nnew mode 100755\n".to_owned()
    ).unwrap();
    assert_eq!(patch.files[0].new_path.as_deref(), Some("\u{1f600}"));
}

#[test]
fn highest_representable_line_number_is_not_incremented_past_the_hunk() {
    let input = format!(
        "--- a/x\n+++ b/x\n@@ -{} +{} @@\n same\n",
        usize::MAX,
        usize::MAX
    );
    let patch = UnifiedPatch::parse(input).unwrap();
    assert_eq!(
        patch.files[0].hunks[0].lines[0].old_number,
        Some(usize::MAX)
    );
}

#[test]
fn separator_like_text_in_git_paths_does_not_split_mode_only_file_names() {
    let patch = UnifiedPatch::parse(
        "diff --git a/space b/name.txt b/space b/name.txt\nold mode 100644\nnew mode 100755\n"
            .to_owned(),
    )
    .unwrap();
    assert_eq!(patch.files[0].old_path.as_deref(), Some("space b/name.txt"));
    assert_eq!(patch.files[0].new_path.as_deref(), Some("space b/name.txt"));
}

#[test]
fn sparse_hunks_do_not_materialize_omitted_file_lines() {
    let patch = UnifiedPatch::parse(
        "--- a/x\n+++ b/x\n@@ -900000000,1 +900000000,2 @@\n keep\n+extra\n".to_owned(),
    )
    .unwrap();
    let hunk = &patch.files[0].hunks[0];
    assert_eq!(hunk.old_start, 900000000);
    assert_eq!(hunk.new_count, 2);
    assert_eq!(hunk.lines.len(), 2);
    assert_eq!(hunk.lines[1].new_number, Some(900000001));
    assert_eq!(patch.text(&hunk.lines[0].text), "keep");
}

#[test]
fn sgr_colours_are_removed_without_losing_unicode_body_text() {
    let patch = UnifiedPatch::parse(
        concat!(
            "\x1b[1mdiff --git a/λ b/λ\x1b[m\n",
            "\x1b[1m--- a/λ\x1b[m\n\x1b[1m+++ b/λ\x1b[m\n",
            "\x1b[36m@@ -1 +1 @@\x1b[m\n",
            "\x1b[31m-old\x1b[m\n\x1b[32m+新\x1b[m\n"
        )
        .to_owned(),
    )
    .unwrap();
    assert_eq!(patch.files[0].new_path.as_deref(), Some("λ"));
    assert_eq!(patch.text(&patch.files[0].hunks[0].lines[1].text), "新");
}

#[test]
fn crlf_and_no_final_newline_markers_attach_to_the_correct_sides() {
    let patch = UnifiedPatch::parse(
        concat!(
            "--- a/x\r\n+++ b/x\r\n@@ -1 +1 @@\r\n-old\r\n",
            "\\ No newline at end of file\r\n+new\r\n\\ No newline at end of file"
        )
        .to_owned(),
    )
    .unwrap();
    let lines = &patch.files[0].hunks[0].lines;
    assert_eq!(patch.text(&lines[0].text), "old");
    assert_eq!(patch.text(&lines[1].text), "new");
    assert!(lines[0].no_newline);
    assert!(lines[1].no_newline);
}

#[test]
fn context_no_final_newline_marker_applies_to_both_original_numbers() {
    let patch = UnifiedPatch::parse(
        "--- a/x\n+++ b/x\n@@ -7 +8 @@\n same\n\\ No newline at end of file\n".to_owned(),
    )
    .unwrap();
    let line = &patch.files[0].hunks[0].lines[0];
    assert_eq!(
        (line.old_number, line.new_number, line.no_newline),
        (Some(7), Some(8), true)
    );
}

#[test]
fn malformed_truncated_overfull_and_unrecognised_content_is_rejected() {
    for input in [
        "not a patch\n",
        "--- a/x\n",
        "--- a/x\n+++ b/x\n",
        "diff --git a/x b/x\nold mode 100644\n",
        "diff --git a/x b/y\nrename from x\n",
        "diff --git a/x b/y\ncopy to y\n",
        "--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n a\n\\ No newline at end of file\n b\n",
        "--- a/x\n+++ b/x\n@@ -1 +1 @@\n a\n\\ No newline at end of file\n@@ -9 +9 @@\n b\n",
        "--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n only one\n",
        "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n",
        "--- a/x\n+++ b/x\n@@ -1 +1 @@\n same\n+extra\n",
        "--- a/x\n+++ b/x\n@@ -1 +1 @@\n same\ntrailing garbage\n",
        "--- a/x\n+++ b/x\n@@ -0 +1 @@\n-old\n+new\n",
        "--- a/x\n+++ b/x\n@@ -1 +1 @@\n\\ No newline at end of file\n-a\n+b\n",
        "--- a/x\n+++ b/x\n@@ -1 +1 @@\n same\n\\ No newline at end of file\n\\ No newline at end of file\n",
        "--- a/x\n+++ b/x\n@@ -one +1 @@\n-a\n+b\n",
        "--- a/x\n+++ b/x\n@@ -1 +1 @\n-a\n+b\n",
        "diff --git a/x b/x\n",
        "diff --git \"a/unterminated b/x\nold mode 100644\nnew mode 100755\n",
        "diff --git a/x b/x\nGIT binary patch\nliteral 3\n",
        "diff --git \"a/\\377\" b/x\nold mode 100644\nnew mode 100755\n",
        "diff --git a/x b/x\nnew file mode 100644\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n",
    ] {
        let error = UnifiedPatch::parse(input.to_owned()).expect_err(input);
        assert!(error.to_string().contains("line"), "{error}");
    }
}

#[test]
fn overlapping_out_of_order_and_overflowing_hunks_are_rejected() {
    let max = usize::MAX;
    for input in [
        format!("--- a/x\n+++ b/x\n@@ -{max},2 +1,2 @@\n a\n b\n"),
        format!("--- a/x\n+++ b/x\n@@ -{max}0 +1 @@\n-a\n+b\n"),
        "--- a/x\n+++ b/x\n@@ -9 +9 @@\n a\n@@ -4 +4 @@\n b\n".to_owned(),
        "--- a/x\n+++ b/x\n@@ -4,2 +4,2 @@\n a\n b\n@@ -5 +5 @@\n c\n".to_owned(),
    ] {
        assert!(UnifiedPatch::parse(input.clone()).is_err(), "{input}");
    }
}

#[test]
fn combined_diff_is_rejected_instead_of_showing_a_partial_patch() {
    for input in [
        "diff --cc file\nindex 123,456..789\n@@@ -1,1 -1,1 +1,1 @@@\n++text\n",
        "diff --combined file\n",
        "--- a/x\n+++ b/x\n@@@ -1 -1 +1 @@@\n++text\n",
        "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\ndiff --cc y\n",
    ] {
        assert!(UnifiedPatch::parse(input.to_owned()).is_err(), "{input}");
    }
}

#[test]
fn raw_terminal_commands_and_nul_are_rejected() {
    for body in ["\x1b]52;c;secret\x07", "\x1b[2J", "\0", "\x08", "\rhidden"] {
        let input = format!("--- a/x\n+++ b/x\n@@ -0,0 +1 @@\n+{body}\n");
        assert!(UnifiedPatch::parse(input).is_err(), "{body:?}");
    }
}

#[test]
fn empty_input_has_no_files() {
    assert!(UnifiedPatch::parse(String::new()).unwrap().files.is_empty());
}

#[test]
fn git_no_prefix_and_plain_unified_paths_keep_real_a_and_b_directories() {
    for (input, old, new) in [
        (
            "diff --git a/file a/file\n--- a/file\n+++ a/file\n@@ -1 +1 @@\n-old\n+new\n",
            "a/file",
            "a/file",
        ),
        (
            "diff --git b/file b/file\nold mode 100644\nnew mode 100755\n",
            "b/file",
            "b/file",
        ),
        (
            "diff --git a/space b/name a/space b/name\nold mode 100644\nnew mode 100755\n",
            "a/space b/name",
            "a/space b/name",
        ),
        (
            "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new\n",
            "a/file",
            "b/file",
        ),
        (
            "diff --git a/file b/file\nsimilarity index 50%\nrename from a/file\nrename to b/file\n--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new\n",
            "a/file",
            "b/file",
        ),
        (
            "diff --git a/file b/file\nsimilarity index 50%\ncopy from a/file\ncopy to b/file\n--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new\n",
            "a/file",
            "b/file",
        ),
    ] {
        let patch = UnifiedPatch::parse(input.to_owned()).unwrap();
        assert_eq!(patch.files[0].old_path.as_deref(), Some(old), "{input}");
        assert_eq!(patch.files[0].new_path.as_deref(), Some(new), "{input}");
    }
    let patch = UnifiedPatch::parse(
        "diff --git b/new b/new\nnew file mode 100644\n--- /dev/null\n+++ b/new\n@@ -0,0 +1 @@\n+new\n".to_owned()
    ).unwrap();
    assert_eq!(patch.files[0].old_path, None);
    assert_eq!(patch.files[0].new_path.as_deref(), Some("b/new"));
}

#[test]
fn binary_sections_require_a_blank_terminator_before_eof_or_another_file() {
    for suffix in ["", "diff --git a/y b/y\nold mode 100644\nnew mode 100755\n"] {
        let input =
            format!("diff --git a/x b/x\nGIT binary patch\nliteral 3\nKcmZQzU|?Vb00001\n{suffix}");
        assert!(UnifiedPatch::parse(input).is_err());
    }
    let patch = UnifiedPatch::parse(
        "diff --git a/x b/x\nGIT binary patch\nliteral 3\nKcmZQzU|?Vb00001\n\n".to_owned(),
    )
    .unwrap();
    assert!(patch.files[0].binary);
}

#[test]
fn empty_hunk_anchors_cannot_extend_past_a_marked_final_line() {
    for input in [
        "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n@@ -5,0 +6 @@\n+extra\n",
        "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n\\ No newline at end of file\n@@ -6 +5,0 @@\n-extra\n",
    ] {
        assert!(UnifiedPatch::parse(input.to_owned()).is_err(), "{input}");
    }
}

#[test]
fn empty_hunks_can_remain_anchored_at_a_marked_final_line() {
    for (input, kind, old_number, new_number) in [
        (
            "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n@@ -1,0 +2 @@\n+extra\n",
            PatchLineKind::Added,
            None,
            Some(2),
        ),
        (
            "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n\\ No newline at end of file\n@@ -2 +1,0 @@\n-extra\n",
            PatchLineKind::Removed,
            Some(2),
            None,
        ),
    ] {
        let patch = UnifiedPatch::parse(input.to_owned()).unwrap();
        let line = &patch.files[0].hunks[1].lines[0];
        assert_eq!(
            (line.kind, line.old_number, line.new_number),
            (kind, old_number, new_number)
        );
        assert_eq!(patch.text(&line.text), "extra");
    }
}
