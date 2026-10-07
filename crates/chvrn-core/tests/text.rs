use chvrn_core::TextSnapshot;
use chvrn_core::diff::{ApplyDirection, Diff, WhitespacePolicy};
use chvrn_core::text::line_range;

#[test]
fn line_coordinates_cover_mixed_terminators_and_terminal_empty_lines() {
    let cases: &[(&str, &[std::ops::Range<usize>])] = &[
        ("", std::slice::from_ref(&(0..0))),
        ("a", std::slice::from_ref(&(0..1))),
        ("a\nb", &[0..1, 2..3]),
        ("a\r\nb", &[0..1, 3..4]),
        ("a\rb", &[0..1, 2..3]),
        ("a\r", &[0..1, 2..2]),
        ("a\n", &[0..1, 2..2]),
        ("a\r\n", &[0..1, 3..3]),
        ("\r\n\r\n\n\r", &[0..0, 2..2, 4..4, 5..5, 6..6]),
        ("é\r\n🦀\ra\nz", &[0..2, 4..8, 9..10, 11..12]),
    ];
    for (text, expected) in cases {
        for (line, range) in expected.iter().enumerate() {
            assert_eq!(
                line_range(text, line),
                Some(range.clone()),
                "{text:?} {line}"
            );
        }
        assert_eq!(line_range(text, expected.len()), None, "{text:?}");
        assert_eq!(line_range(text, usize::MAX), None, "{text:?}");
    }
}

#[test]
fn unicode_separators_remain_content_in_byte_coordinates() {
    for separator in ['\u{000b}', '\u{000c}', '\u{0085}', '\u{2028}', '\u{2029}'] {
        let text = format!("é{separator}🦀\nc");
        let first_end = 2 + separator.len_utf8() + 4;
        assert_eq!(line_range(&text, 0), Some(0..first_end));
        assert_eq!(line_range(&text, 1), Some(first_end + 1..first_end + 2));
        assert_eq!(line_range(&text, 2), None);
    }
}

#[test]
fn terminal_coordinate_does_not_add_diff_rows_and_eof_hunks_still_apply() {
    let empty = TextSnapshot::from_bytes(b"").unwrap();
    let before = TextSnapshot::from_bytes(b"a\r").unwrap();
    let after = TextSnapshot::from_bytes(b"a\rb\r").unwrap();
    assert!(
        Diff::between(&empty, &empty, WhitespacePolicy::Exact)
            .rows()
            .is_empty()
    );
    assert_eq!(
        Diff::between(&before, &before, WhitespacePolicy::Exact)
            .rows()
            .len(),
        1
    );
    assert_eq!(line_range(before.text(), 1), Some(2..2));
    let diff = Diff::between(&before, &after, WhitespacePolicy::Exact);
    assert_eq!(diff.hunks().len(), 1);
    assert_eq!(diff.hunks()[0].left_lines, 1..1);
    assert_eq!(diff.rows().len(), 2);
    let applied = diff
        .apply_hunk(&diff.hunks()[0], &before, ApplyDirection::RightToLeft)
        .unwrap();
    assert_eq!(applied.text(), after.text());
}
