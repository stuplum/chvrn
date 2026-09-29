use chvrn_core::diff::{
    ApplyDirection, ApplyError, Diff, IntralineChange, WhitespacePolicy, intraline_spans,
};
use chvrn_core::{TextError, TextSnapshot};

fn snapshot(text: &str) -> TextSnapshot {
    TextSnapshot::from_bytes(text.as_bytes()).unwrap()
}

#[test]
fn patience_keeps_unique_lines_aligned_when_a_different_line_moves_past_them() {
    let left = snapshot("start\nA\nB\nX\nend\n");
    let right = snapshot("start\nX\nA\nB\nend\n");
    let diff = Diff::between(&left, &right, WhitespacePolicy::Exact);
    let rows: Vec<_> = diff
        .rows()
        .iter()
        .map(|row| {
            (
                row.left
                    .as_ref()
                    .map(|line| (line.number, line.text.as_str())),
                row.right
                    .as_ref()
                    .map(|line| (line.number, line.text.as_str())),
            )
        })
        .collect();

    assert_eq!(
        rows,
        vec![
            (Some((1, "start")), Some((1, "start"))),
            (None, Some((2, "X"))),
            (Some((2, "A")), Some((3, "A"))),
            (Some((3, "B")), Some((4, "B"))),
            (Some((4, "X")), None),
            (Some((5, "end")), Some((5, "end"))),
        ]
    );
    assert_eq!(diff.hunks().len(), 2);
    assert_eq!(diff.hunks()[0].left_lines, 1..1);
    assert_eq!(diff.hunks()[0].right_lines, 1..2);
    assert_eq!(diff.hunks()[1].left_lines, 3..4);
    assert_eq!(diff.hunks()[1].right_lines, 4..4);
}

#[test]
fn unequal_replacement_uses_virtual_filler_without_adding_a_real_blank_line() {
    let left = snapshot("head\nold1\nold2\ntail\n");
    let right = snapshot("head\nnew\ntail\n");
    let diff = Diff::between(&left, &right, WhitespacePolicy::Exact);

    assert_eq!(diff.rows()[1].left.as_ref().unwrap().text, "old1");
    assert_eq!(diff.rows()[1].right.as_ref().unwrap().text, "new");
    assert_eq!(diff.rows()[2].left.as_ref().unwrap().number, 3);
    assert!(diff.rows()[2].right.is_none());
    assert_eq!(diff.rows()[3].right.as_ref().unwrap().number, 3);
    assert_eq!(left.as_bytes(), b"head\nold1\nold2\ntail\n");
    assert_eq!(right.as_bytes(), b"head\nnew\ntail\n");
}

#[test]
fn whitespace_matching_modes_do_not_rewrite_the_original_bytes() {
    let edge_left = snapshot(" \ta \n");
    let edge_right = snapshot("a\n");
    assert!(
        !Diff::between(&edge_left, &edge_right, WhitespacePolicy::Exact)
            .hunks()
            .is_empty()
    );
    assert!(
        Diff::between(&edge_left, &edge_right, WhitespacePolicy::IgnoreEdge)
            .hunks()
            .is_empty()
    );
    assert_eq!(edge_left.as_bytes(), b" \ta \n");
    assert_eq!(edge_right.as_bytes(), b"a\n");

    let all_left = snapshot("a b\n");
    let all_right = snapshot("ab\n");
    assert!(
        !Diff::between(&all_left, &all_right, WhitespacePolicy::IgnoreEdge)
            .hunks()
            .is_empty()
    );
    assert!(
        Diff::between(&all_left, &all_right, WhitespacePolicy::IgnoreAll)
            .hunks()
            .is_empty()
    );
    assert!(
        !Diff::between(&all_left, &all_right, WhitespacePolicy::IgnoreBlankLines)
            .hunks()
            .is_empty()
    );
    assert_eq!(all_left.as_bytes(), b"a b\n");

    let blank_left = snapshot("a\n \nb\n");
    let blank_right = snapshot("a\nb\n");
    let blank_diff = Diff::between(
        &blank_left,
        &blank_right,
        WhitespacePolicy::IgnoreBlankLines,
    );
    assert!(blank_diff.hunks().is_empty());
    assert_eq!(blank_diff.rows()[1].left.as_ref().unwrap().text, " ");
    assert!(blank_diff.rows()[1].right.is_none());
    assert_eq!(blank_left.as_bytes(), b"a\n \nb\n");
}

#[test]
fn applying_a_hunk_under_ignore_edge_keeps_unselected_destination_whitespace() {
    let left = snapshot(" keep \nold\n");
    let right = snapshot("keep\nnew\n");
    let diff = Diff::between(&left, &right, WhitespacePolicy::IgnoreEdge);

    assert_eq!(diff.hunks().len(), 1);
    assert_eq!(
        diff.apply_hunk(&diff.hunks()[0], &right, ApplyDirection::LeftToRight)
            .unwrap()
            .as_bytes(),
        b"keep\nold\n"
    );
    assert_eq!(
        diff.apply_hunk(&diff.hunks()[0], &left, ApplyDirection::RightToLeft)
            .unwrap()
            .as_bytes(),
        b" keep \nnew\n"
    );
}

#[test]
fn applying_a_middle_hunk_either_way_retains_crlf_and_absent_final_newline() {
    let left = snapshot("head\r\nold\r\ntail");
    let right = snapshot("head\r\nnew\r\ntail");
    let diff = Diff::between(&left, &right, WhitespacePolicy::Exact);
    assert_eq!(diff.hunks().len(), 1);

    let copied_left = diff
        .apply_hunk(&diff.hunks()[0], &right, ApplyDirection::LeftToRight)
        .unwrap();
    let copied_right = diff
        .apply_hunk(&diff.hunks()[0], &left, ApplyDirection::RightToLeft)
        .unwrap();
    assert_eq!(copied_left.as_bytes(), b"head\r\nold\r\ntail");
    assert_eq!(copied_right.as_bytes(), b"head\r\nnew\r\ntail");
}

#[test]
fn boundary_hunks_insert_and_delete_utf8_text_in_both_directions() {
    let empty = snapshot("");
    let content = snapshot("é\r\n");
    let diff = Diff::between(&empty, &content, WhitespacePolicy::Exact);
    assert_eq!(diff.hunks().len(), 1);
    assert_eq!(diff.hunks()[0].left_lines, 0..0);
    assert_eq!(diff.hunks()[0].right_lines, 0..1);
    assert_eq!(
        diff.apply_hunk(&diff.hunks()[0], &content, ApplyDirection::LeftToRight)
            .unwrap()
            .as_bytes(),
        b""
    );
    assert_eq!(
        diff.apply_hunk(&diff.hunks()[0], &empty, ApplyDirection::RightToLeft)
            .unwrap()
            .as_bytes(),
        "é\r\n".as_bytes()
    );

    let before = snapshot("keep\nlast");
    let after = snapshot("keep\n");
    let end_diff = Diff::between(&before, &after, WhitespacePolicy::Exact);
    assert_eq!(end_diff.hunks()[0].left_lines, 1..2);
    assert_eq!(end_diff.hunks()[0].right_lines, 1..1);
    assert_eq!(
        end_diff
            .apply_hunk(&end_diff.hunks()[0], &after, ApplyDirection::LeftToRight)
            .unwrap()
            .as_bytes(),
        b"keep\nlast"
    );
    assert_eq!(
        end_diff
            .apply_hunk(&end_diff.hunks()[0], &before, ApplyDirection::RightToLeft)
            .unwrap()
            .as_bytes(),
        b"keep\n"
    );
}

#[test]
fn removing_only_the_final_newline_is_an_exact_reversible_change() {
    let left = snapshot("line\n");
    let right = snapshot("line");
    let diff = Diff::between(&left, &right, WhitespacePolicy::Exact);
    assert_eq!(diff.hunks().len(), 1);
    assert_eq!(
        diff.apply_hunk(&diff.hunks()[0], &right, ApplyDirection::LeftToRight)
            .unwrap()
            .as_bytes(),
        b"line\n"
    );
    assert_eq!(
        diff.apply_hunk(&diff.hunks()[0], &left, ApplyDirection::RightToLeft)
            .unwrap()
            .as_bytes(),
        b"line"
    );
}

#[test]
fn a_hunk_rejects_an_independently_created_snapshot_even_when_its_bytes_match() {
    let left = snapshot("old\n");
    let right = snapshot("new\n");
    let diff = Diff::between(&left, &right, WhitespacePolicy::Exact);
    let reconstructed = snapshot("new\n");
    assert!(matches!(
        diff.apply_hunk(
            &diff.hunks()[0],
            &reconstructed,
            ApplyDirection::LeftToRight
        ),
        Err(ApplyError::StaleSnapshot)
    ));
    assert_eq!(reconstructed.as_bytes(), b"new\n");

    let newer = snapshot("edited\n");
    assert!(matches!(
        diff.apply_hunk(&diff.hunks()[0], &newer, ApplyDirection::LeftToRight),
        Err(ApplyError::StaleSnapshot)
    ));
    assert_eq!(newer.as_bytes(), b"edited\n");
}

#[test]
fn a_hunk_from_a_different_comparison_cannot_be_applied_to_this_diff() {
    let left = snapshot("one\n");
    let right = snapshot("two\n");
    let another = snapshot("three\n");
    let diff = Diff::between(&left, &right, WhitespacePolicy::Exact);
    let unrelated = Diff::between(&left, &another, WhitespacePolicy::Exact);
    assert!(matches!(
        diff.apply_hunk(&unrelated.hunks()[0], &right, ApplyDirection::LeftToRight),
        Err(ApplyError::ForeignHunk)
    ));
    assert_eq!(right.as_bytes(), b"two\n");
}

#[test]
fn intraline_ranges_count_unicode_scalars_not_utf8_bytes() {
    assert_eq!(
        intraline_spans("a🦀z", "a🦊z"),
        vec![IntralineChange {
            left: 1..2,
            right: 1..2,
        }]
    );
    assert_eq!(
        intraline_spans("e\u{301}x", "éx"),
        vec![IntralineChange {
            left: 0..2,
            right: 0..1,
        }]
    );
}

#[test]
fn text_ingestion_refuses_invalid_utf8_instead_of_replacing_bytes() {
    assert!(matches!(
        TextSnapshot::from_bytes(&[b'a', 0xff, b'b']),
        Err(TextError::InvalidUtf8)
    ));
}

#[test]
fn text_ingestion_identifies_nul_containing_input_as_binary() {
    assert!(matches!(
        TextSnapshot::from_bytes(b"valid utf8\0still utf8"),
        Err(TextError::BinaryInput)
    ));
}
