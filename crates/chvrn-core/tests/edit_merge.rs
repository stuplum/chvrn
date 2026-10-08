use chvrn_core::TextSnapshot;
use chvrn_core::diff::{ApplyDirection, ApplyError, Diff, WhitespacePolicy};
use chvrn_core::edit::{CursorMotion, EditError, TextBuffer};
use chvrn_core::merge::{ConflictId, ConflictResolution, Merge, ResolveError};

fn snapshot(text: &str) -> TextSnapshot {
    TextSnapshot::from_bytes(text.as_bytes()).unwrap()
}

#[test]
fn rope_edits_preserve_utf8_crlf_and_cursor_through_undo_redo() {
    let mut buffer = TextBuffer::new(snapshot("a\r\nz"));
    buffer.set_cursor(1).unwrap();
    buffer.insert("🦀").unwrap();
    assert_eq!(buffer.text(), "a🦀\r\nz");
    assert_eq!(buffer.cursor(), 2);
    assert_eq!(buffer.snapshot().as_bytes(), "a🦀\r\nz".as_bytes());

    assert!(buffer.undo());
    assert_eq!(buffer.text(), "a\r\nz");
    assert_eq!(buffer.cursor(), 1);
    assert!(buffer.redo());
    assert_eq!(buffer.text(), "a🦀\r\nz");
    assert_eq!(buffer.cursor(), 2);

    buffer.delete(1..2).unwrap();
    assert_eq!(buffer.text(), "a\r\nz");
    assert_eq!(buffer.cursor(), 1);
    assert!(buffer.undo());
    assert_eq!(buffer.text(), "a🦀\r\nz");
    assert_eq!(buffer.cursor(), 2);
}

#[test]
fn snapshot_identity_stays_stable_between_reads_but_not_across_undo_to_equal_bytes() {
    let mut buffer = TextBuffer::new(snapshot("a\n"));
    let original = buffer.snapshot();
    assert!(buffer.snapshot().same_identity(&original));

    buffer.insert("x").unwrap();
    let edited = buffer.snapshot();
    assert_eq!(edited.as_bytes(), b"xa\n");
    assert!(!edited.same_identity(&original));
    assert!(buffer.snapshot().same_identity(&edited));

    assert!(buffer.undo());
    let undone = buffer.snapshot();
    assert_eq!(undone.as_bytes(), original.as_bytes());
    assert!(!undone.same_identity(&original));
    assert!(!undone.same_identity(&edited));

    assert!(buffer.redo());
    let redone = buffer.snapshot();
    assert_eq!(redone.as_bytes(), edited.as_bytes());
    assert!(!redone.same_identity(&edited));
}

#[test]
fn capture_preserves_the_edit_identity_when_flattened_later_on_a_worker() {
    let mut buffer = TextBuffer::new(snapshot("é\r\nlast"));
    let original = buffer.snapshot();
    let captured_before = buffer.capture();

    buffer.insert("🦀").unwrap();
    let captured_after = buffer.capture();
    let captured_again = buffer.capture();
    let edited = std::thread::spawn(move || captured_after.snapshot())
        .join()
        .unwrap();

    assert_eq!(captured_before.snapshot().as_bytes(), original.as_bytes());
    assert_eq!(edited.as_bytes(), "🦀é\r\nlast".as_bytes());
    assert!(!edited.same_identity(&original));
    assert!(edited.same_identity(&captured_again.snapshot()));
    assert!(edited.same_identity(&buffer.snapshot()));

    assert!(buffer.undo());
    let restored = buffer.capture().snapshot();
    assert_eq!(restored.as_bytes(), original.as_bytes());
    assert!(!restored.same_identity(&original));
    assert!(!restored.same_identity(&edited));
}

#[test]
fn captured_identity_rejects_stale_async_results_without_flattening_newer_edits() {
    let mut buffer = TextBuffer::new(snapshot("old\n"));
    let requested = buffer.snapshot();
    let original_capture = buffer.capture();
    assert!(original_capture.same_identity(&requested));

    buffer.insert("x").unwrap();
    assert!(original_capture.same_identity(&requested));
    assert!(!buffer.capture().same_identity(&requested));

    assert!(buffer.undo());
    let restored = buffer.capture();
    assert_eq!(restored.clone().snapshot().as_bytes(), requested.as_bytes());
    assert!(!restored.same_identity(&requested));
}

#[test]
fn visible_line_accessors_keep_unicode_offsets_and_crlf_without_flattening_the_buffer() {
    let buffer = TextBuffer::new(snapshot("é🦀\r\nq"));

    assert_eq!(buffer.len_bytes(), 9);
    assert_eq!(buffer.len_chars(), 5);
    assert_eq!(buffer.line_count(), 2);
    assert_eq!(buffer.char_to_line(4), Ok(1));
    assert_eq!(buffer.line_to_char(1), Some(4));
    assert_eq!(buffer.line_text(0).as_deref(), Some("é🦀\r\n"));
    assert_eq!(buffer.slice_chars(0..2).as_deref(), Ok("é🦀"));
    assert_eq!(buffer.line_to_char(2), None);
    assert_eq!(buffer.char_to_line(6), Err(EditError::OutOfBounds));
}

#[test]
fn editor_treats_unicode_separators_as_content_and_only_cr_lf_as_lines() {
    for separator in ['\u{000b}', '\u{000c}', '\u{0085}', '\u{2028}', '\u{2029}'] {
        let mut buffer = TextBuffer::new(snapshot(&format!("a{separator}b\nc")));
        assert_eq!(buffer.line_count(), 2, "{separator:?}");
        assert_eq!(buffer.line_to_char(1), Some(4), "{separator:?}");
        buffer.set_cursor(buffer.line_to_char(1).unwrap()).unwrap();
        buffer.insert("X").unwrap();
        assert_eq!(buffer.text(), format!("a{separator}b\nXc"));
    }
    let buffer = TextBuffer::new(snapshot("a\rb\r\nc\n"));
    assert_eq!(buffer.line_count(), 4);
    assert_eq!(buffer.line_to_char(1), Some(2));
    assert_eq!(buffer.line_to_char(2), Some(5));
    assert_eq!(buffer.line_to_char(3), Some(7));
}

#[test]
fn a_new_edit_after_undo_clears_redo_without_changing_original_line_endings() {
    let mut buffer = TextBuffer::new(snapshot("a\r\nz"));
    buffer.set_cursor(1).unwrap();
    buffer.insert("x").unwrap();
    assert!(buffer.undo());
    buffer.insert("é").unwrap();

    assert_eq!(buffer.text(), "aé\r\nz");
    assert!(!buffer.redo());
    assert_eq!(buffer.snapshot().as_bytes(), "aé\r\nz".as_bytes());
}

#[test]
fn replacing_a_range_is_one_undoable_operation() {
    let mut buffer = TextBuffer::new(snapshot("abcd"));
    buffer.set_cursor(1).unwrap();
    buffer.replace(1..3, "🦀").unwrap();
    assert_eq!(buffer.text(), "a🦀d");
    assert_eq!(buffer.cursor(), 2);

    assert!(buffer.undo());
    assert_eq!(buffer.text(), "abcd");
    assert_eq!(buffer.cursor(), 1);
    assert!(buffer.redo());
    assert_eq!(buffer.text(), "a🦀d");
    assert_eq!(buffer.cursor(), 2);
}

#[test]
fn cursor_motion_uses_character_columns_and_never_lands_inside_crlf() {
    let mut buffer = TextBuffer::new(snapshot("é🦀\r\nq\r\nlast"));
    buffer.set_cursor(2).unwrap();
    assert!(buffer.move_cursor(CursorMotion::Right));
    assert_eq!(buffer.cursor(), 4);
    assert!(buffer.move_cursor(CursorMotion::Left));
    assert_eq!(buffer.cursor(), 2);
    assert!(buffer.move_cursor(CursorMotion::Down));
    assert_eq!(buffer.cursor(), 5);
    assert!(buffer.move_cursor(CursorMotion::Down));
    assert_eq!(buffer.cursor(), 9);
    assert!(buffer.move_cursor(CursorMotion::Up));
    assert_eq!(buffer.cursor(), 5);
    assert!(buffer.move_cursor(CursorMotion::Up));
    assert_eq!(buffer.cursor(), 2);
    assert!(buffer.move_cursor(CursorMotion::Left));
    assert_eq!(buffer.cursor(), 1);
    assert_eq!(buffer.text(), "é🦀\r\nq\r\nlast");
}

#[test]
fn invalid_character_boundaries_leave_rope_and_undo_history_unchanged() {
    let mut buffer = TextBuffer::new(snapshot("é\r\nz"));
    assert_eq!(
        buffer.set_cursor(2),
        Err(EditError::InvalidLineEndingBoundary)
    );
    assert_eq!(
        buffer.delete(1..2),
        Err(EditError::InvalidLineEndingBoundary)
    );
    assert_eq!(buffer.delete(0..99), Err(EditError::OutOfBounds));
    assert_eq!(buffer.text(), "é\r\nz");
    assert_eq!(buffer.cursor(), 0);
    assert!(!buffer.undo());
}

#[test]
fn a_snapshot_taken_before_an_edit_cannot_authorise_a_hunk_on_the_edited_rope() {
    let mut buffer = TextBuffer::new(snapshot("old\n"));
    let inspected = buffer.snapshot();
    let proposed = snapshot("new\n");
    let diff = Diff::between(&proposed, &inspected, WhitespacePolicy::Exact);
    buffer.set_cursor(0).unwrap();
    buffer.insert("x").unwrap();
    let edited = buffer.snapshot();

    assert!(matches!(
        diff.apply_hunk(&diff.hunks()[0], &edited, ApplyDirection::LeftToRight),
        Err(ApplyError::StaleSnapshot)
    ));
    assert_eq!(buffer.text(), "xold\n");
}

#[test]
fn independent_three_way_edits_combine_with_exact_crlf_and_final_newline_state() {
    let base = snapshot("one\r\ntwo\r\nthree");
    let ours = snapshot("ONE\r\ntwo\r\nthree");
    let theirs = snapshot("one\r\ntwo\r\nTHREE");
    let merge = Merge::three_way(&base, &ours, &theirs);

    assert!(merge.conflicts().is_empty());
    assert_eq!(merge.result().unwrap().as_bytes(), b"ONE\r\ntwo\r\nTHREE");
}

#[test]
fn independent_changes_on_the_same_line_combine_without_a_conflict() {
    let merge = Merge::three_way(
        &snapshot("left middle right\n"),
        &snapshot("LEFT middle right\n"),
        &snapshot("left middle RIGHT\n"),
    );
    assert!(merge.conflicts().is_empty());
    assert_eq!(merge.result().unwrap().as_bytes(), b"LEFT middle RIGHT\n");
}

#[test]
fn independent_changes_across_the_same_multiline_hunk_combine_without_a_conflict() {
    let merge = Merge::three_way(
        &snapshot("left a\nright b\n"),
        &snapshot("LEFT a\nRIGHT b\n"),
        &snapshot("left A\nright B\n"),
    );

    assert!(merge.conflicts().is_empty());
    assert_eq!(merge.result().unwrap().as_bytes(), b"LEFT A\nRIGHT B\n");
}

#[test]
fn identical_changes_on_both_sides_are_not_duplicated() {
    let merge = Merge::three_way(&snapshot("old\n"), &snapshot("new\n"), &snapshot("new\n"));
    assert!(merge.conflicts().is_empty());
    assert_eq!(merge.result().unwrap().as_bytes(), b"new\n");
}

fn overlapping_merge() -> Merge {
    Merge::three_way(
        &snapshot("top\nkeep1\nkeep2\nvalue\nkeep3\nkeep4\nbottom\n"),
        &snapshot("TOP\nkeep1\nkeep2\nours\nkeep3\nkeep4\nbottom\n"),
        &snapshot("top\nkeep1\nkeep2\ntheirs\nkeep3\nkeep4\nBOTTOM\n"),
    )
}

#[test]
fn overlapping_edits_remain_unresolved_without_discarding_independent_edits() {
    let merge = overlapping_merge();
    assert_eq!(merge.conflicts().len(), 1);
    assert!(merge.result().is_none());
}

#[test]
fn unresolved_preview_marks_only_ours_provisional_region_amid_independent_edits() {
    let merge = overlapping_merge();
    assert_eq!(
        merge.preview().as_bytes(),
        b"TOP\nkeep1\nkeep2\nours\nkeep3\nkeep4\nBOTTOM\n"
    );
    let conflicts = merge.preview_conflicts();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].id, merge.conflicts()[0].id);
    assert_eq!(conflicts[0].result_chars, 16..21);
    assert_eq!(conflicts[0].result_lines, 3..4);
    assert_eq!(merge.conflicts()[0].base_lines, 3..4);
    assert_eq!(merge.conflicts()[0].ours_lines, 3..4);
    assert_eq!(merge.conflicts()[0].theirs_lines, 3..4);
}

#[test]
fn resolving_an_earlier_conflict_shifts_later_preview_ranges() {
    let mut merge = Merge::three_way(
        &snapshot("a\nmid\nb\n"),
        &snapshot("A\nmid\nB\n"),
        &snapshot("X\nmid\nY\n"),
    );
    assert_eq!(merge.preview_conflicts().len(), 2);
    assert_eq!(merge.preview_conflicts()[1].result_chars, 6..8);
    let first = merge.preview_conflicts()[0].id;
    merge.resolve(first, ConflictResolution::Both).unwrap();
    assert_eq!(merge.preview().as_bytes(), b"A\nX\nmid\nB\n");
    let remaining = merge.preview_conflicts();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].result_chars, 8..10);
    assert_eq!(remaining[0].result_lines, 3..4);
    assert!(merge.result().is_none());
}

#[test]
fn a_deletion_conflict_has_an_empty_editable_preview_span() {
    let mut merge = Merge::three_way(&snapshot("x\n"), &snapshot(""), &snapshot("y\n"));
    assert_eq!(merge.preview().as_bytes(), b"");
    assert_eq!(merge.preview_conflicts().len(), 1);
    assert_eq!(merge.preview_conflicts()[0].result_chars, 0..0);
    assert_eq!(merge.preview_conflicts()[0].result_lines, 0..0);
    assert!(merge.result().is_none());

    let id = merge.preview_conflicts()[0].id;
    merge.resolve(id, ConflictResolution::Theirs).unwrap();
    assert_eq!(merge.result().unwrap().as_bytes(), b"y\n");
}

#[test]
fn deletion_conflict_at_eof_points_after_the_final_newline() {
    let merge = Merge::three_way(
        &snapshot("head\nlast\n"),
        &snapshot("head\n"),
        &snapshot("head\nOTHER\n"),
    );
    assert_eq!(merge.preview().as_bytes(), b"head\n");
    assert_eq!(merge.preview_conflicts().len(), 1);
    assert_eq!(merge.preview_conflicts()[0].result_chars, 5..5);
    assert_eq!(merge.preview_conflicts()[0].result_lines, 1..1);
}

#[test]
fn selecting_ours_resolves_only_the_conflict_and_keeps_their_independent_edit() {
    let mut merge = overlapping_merge();
    let id = merge.conflicts()[0].id;
    merge.resolve(id, ConflictResolution::Ours).unwrap();
    assert_eq!(
        merge.result().unwrap().as_bytes(),
        b"TOP\nkeep1\nkeep2\nours\nkeep3\nkeep4\nBOTTOM\n"
    );
}

#[test]
fn selecting_theirs_resolves_only_the_conflict_and_keeps_our_independent_edit() {
    let mut merge = overlapping_merge();
    let id = merge.conflicts()[0].id;
    merge.resolve(id, ConflictResolution::Theirs).unwrap();
    assert_eq!(
        merge.result().unwrap().as_bytes(),
        b"TOP\nkeep1\nkeep2\ntheirs\nkeep3\nkeep4\nBOTTOM\n"
    );
}

#[test]
fn selecting_both_orders_ours_before_theirs_without_duplicating_other_lines() {
    let mut merge = overlapping_merge();
    let id = merge.conflicts()[0].id;
    merge.resolve(id, ConflictResolution::Both).unwrap();
    assert_eq!(
        merge.result().unwrap().as_bytes(),
        b"TOP\nkeep1\nkeep2\nours\ntheirs\nkeep3\nkeep4\nBOTTOM\n"
    );
}

#[test]
fn manual_conflict_text_becomes_the_result_without_automatic_markers() {
    let mut merge = overlapping_merge();
    let id = merge.conflicts()[0].id;
    merge
        .resolve(id, ConflictResolution::Manual("agreed 🦀\n".to_owned()))
        .unwrap();
    assert_eq!(
        merge.result().unwrap().as_bytes(),
        "TOP\nkeep1\nkeep2\nagreed 🦀\nkeep3\nkeep4\nBOTTOM\n".as_bytes()
    );
}

#[test]
fn unknown_conflict_selection_does_not_resolve_the_actual_conflict() {
    let mut merge = overlapping_merge();
    assert_eq!(
        merge.resolve(ConflictId(999), ConflictResolution::Ours),
        Err(ResolveError::UnknownConflict)
    );
    assert!(merge.result().is_none());
}

#[test]
fn conflict_source_ranges_cover_complete_choices_and_resolve_to_the_same_text() {
    let cases = [
        ("a\nb\n", "A\nb\n", "X\nY\n"),
        ("a\nb\n", "X\nY\n", "A\nb\n"),
        ("a\nb\n", "", "X\nb\n"),
        ("a\nb\n", "X\nb\n", ""),
        ("a\nb\n", "a\n", "X\nY\n"),
        ("a\nb\n", "X\nY\n", "a\n"),
        ("a\nb\n", "A\nINSERTED\nb\n", "X\nY\n"),
        ("a\nb\nc\nd\ne\n", "A\nB\nc\nD\nE\n", "a\nX\nY\nZ\ne\n"),
        ("a\nb\nc\n", "a\ninserted\nb\nc\n", "XYZ\n"),
        ("", "ours\n", "theirs\n"),
    ];
    for (base, ours, theirs) in cases {
        let mut merge = Merge::three_way(&snapshot(base), &snapshot(ours), &snapshot(theirs));
        assert_eq!(merge.conflicts().len(), 1, "{base:?} {ours:?} {theirs:?}");
        let conflict = &merge.conflicts()[0];
        let ours_lines: Vec<_> = ours.split_inclusive('\n').collect();
        let theirs_lines: Vec<_> = theirs.split_inclusive('\n').collect();
        let ours_choice = ours_lines[conflict.ours_lines.clone()].concat();
        let theirs_choice = theirs_lines[conflict.theirs_lines.clone()].concat();
        assert_eq!(ours_choice, ours, "{base:?}");
        assert_eq!(theirs_choice, theirs, "{base:?}");
        let id = conflict.id;
        merge.resolve(id, ConflictResolution::Ours).unwrap();
        assert_eq!(merge.result().unwrap().text(), ours_choice);
        merge.resolve(id, ConflictResolution::Theirs).unwrap();
        assert_eq!(merge.result().unwrap().text(), theirs_choice);
        merge.resolve(id, ConflictResolution::Both).unwrap();
        assert_eq!(
            merge.result().unwrap().text(),
            format!("{ours_choice}{theirs_choice}")
        );
    }
}

#[test]
fn conflict_source_projection_keeps_prior_insertions_and_adjacent_edits_separate() {
    let mut merge = Merge::three_way(
        &snapshot("head\na\nb\ntail\n"),
        &snapshot("extra\nhead\nA\nb\ntail\n"),
        &snapshot("head\nX\nY\ntail\n"),
    );
    assert_eq!(merge.conflicts().len(), 1);
    let conflict = &merge.conflicts()[0];
    let ours = ["extra\n", "head\n", "A\n", "b\n", "tail\n"];
    let theirs = ["head\n", "X\n", "Y\n", "tail\n"];
    assert_eq!(ours[conflict.ours_lines.clone()].concat(), "A\nb\n");
    assert_eq!(theirs[conflict.theirs_lines.clone()].concat(), "X\nY\n");
    let id = conflict.id;
    merge.resolve(id, ConflictResolution::Theirs).unwrap();
    assert_eq!(merge.result().unwrap().text(), "extra\nhead\nX\nY\ntail\n");

    let merge = Merge::three_way(
        &snapshot("a\nb\n"),
        &snapshot("A\nb\n"),
        &snapshot("a\nB\n"),
    );
    assert!(merge.conflicts().is_empty());
    assert_eq!(merge.result().unwrap().text(), "A\nB\n");
}

#[test]
fn insertion_conflicts_at_eof_select_only_inserted_source_lines() {
    let mut merge = Merge::three_way(
        &snapshot("head\n"),
        &snapshot("head\nours\n"),
        &snapshot("head\ntheirs\n"),
    );
    assert_eq!(merge.conflicts().len(), 1);
    let conflict = &merge.conflicts()[0];
    assert_eq!(conflict.base_lines, 1..1);
    let ours = ["head\n", "ours\n"];
    let theirs = ["head\n", "theirs\n"];
    assert_eq!(ours[conflict.ours_lines.clone()].concat(), "ours\n");
    assert_eq!(theirs[conflict.theirs_lines.clone()].concat(), "theirs\n");
    let id = conflict.id;
    merge.resolve(id, ConflictResolution::Both).unwrap();
    assert_eq!(merge.result().unwrap().text(), "head\nours\ntheirs\n");
}
