mod support;

use chvrn_git::{Base, GitError, Repository};
use std::path::{Path, PathBuf};
use support::Fixture;

#[test]
fn reviewed_file_exposes_separate_revision_index_and_worktree_bytes() {
    let fixture = Fixture::new();
    fixture.write("file.txt", b"base\r\n");
    fixture.commit_all("base");
    let base = fixture.head();
    fixture.write("file.txt", b"staged\r\n");
    fixture.git(&["add", "--", "file.txt"]);
    fixture.write("file.txt", b"worktree\r\n");
    let repo = Repository::open(fixture.root()).expect("open repository");

    let review = repo
        .review(Base::Revision(base), &[PathBuf::from("file.txt")])
        .expect("inspect three versions");
    let file = review.file(Path::new("file.txt")).expect("reviewed file");
    assert_eq!(file.base.as_deref(), Some(b"base\r\n".as_slice()));
    assert_eq!(file.index.as_deref(), Some(b"staged\r\n".as_slice()));
    assert_eq!(file.worktree.as_deref(), Some(b"worktree\r\n".as_slice()));
    let tree =
        String::from_utf8(fixture.git(&["rev-parse", "HEAD^{tree}"])).expect("ASCII tree ID");
    assert_eq!(review.resolved_revision(), Some(tree.trim()));
}

#[test]
fn guarded_save_writes_edit_without_staging_and_refuses_reusing_the_snapshot() {
    let fixture = Fixture::new();
    fixture.write("file.txt", b"base\n");
    fixture.commit_all("base");
    let repo = Repository::open(fixture.root()).expect("open repository");
    let review = repo
        .review(Base::Index, &[PathBuf::from("file.txt")])
        .expect("inspect worktree");

    repo.save_worktree(&review, Path::new("file.txt"), b"edited\r\n")
        .expect("save edited buffer");
    assert_eq!(fixture.read("file.txt"), b"edited\r\n");
    assert_eq!(fixture.git(&["show", ":file.txt"]), b"base\n");
    assert!(matches!(
        repo.save_worktree(&review, Path::new("file.txt"), b"stale edit\n"),
        Err(GitError::StaleReview)
    ));
    assert_eq!(fixture.read("file.txt"), b"edited\r\n");
}

#[test]
fn unedited_review_validation_rejects_a_newer_index_snapshot() {
    let fixture = Fixture::new();
    fixture.write("file.txt", b"base\n");
    fixture.commit_all("base");
    let repo = Repository::open(fixture.root()).expect("open repository");
    let review = repo
        .review(Base::Revision(fixture.head()), &[PathBuf::from("file.txt")])
        .expect("inspect unedited session");
    repo.validate_review(&review).expect("current snapshot");
    fixture.write("file.txt", b"new staged contents\n");
    fixture.git(&["add", "--", "file.txt"]);
    fixture.write("file.txt", b"base\n");

    assert!(matches!(
        repo.validate_review(&review),
        Err(GitError::StaleReview)
    ));
    assert_eq!(
        fixture.git(&["show", ":file.txt"]),
        b"new staged contents\n"
    );
    assert_eq!(fixture.read("file.txt"), b"base\n");
}

#[test]
fn multifile_save_preflights_every_path_before_writing_any_file() {
    let fixture = Fixture::new();
    fixture.write("first.txt", b"first base\n");
    fixture.write("second.txt", b"second base\n");
    fixture.commit_all("base");
    let repo = Repository::open(fixture.root()).expect("open repository");
    let review = repo
        .review(
            Base::Index,
            &[PathBuf::from("first.txt"), PathBuf::from("second.txt")],
        )
        .expect("inspect both files");
    fixture.write("second.txt", b"concurrent\n");

    let result = repo.save_files(
        &review,
        &[
            (PathBuf::from("first.txt"), b"first edited\n".to_vec()),
            (PathBuf::from("second.txt"), b"second edited\n".to_vec()),
        ],
    );
    assert!(matches!(result, Err(GitError::StaleReview)));
    assert_eq!(fixture.read("first.txt"), b"first base\n");
    assert_eq!(fixture.read("second.txt"), b"concurrent\n");
}

#[test]
fn discovery_from_nested_directory_returns_the_repository_root() {
    let fixture = Fixture::new();
    fixture.write("nested/file.txt", b"base\n");
    fixture.commit_all("base");

    let repo = Repository::discover(&fixture.root().join("nested"))
        .expect("discover containing repository");
    assert_eq!(
        repo.root(),
        fixture
            .root()
            .canonicalize()
            .expect("canonical repository root")
            .as_path()
    );
}

#[test]
fn hunk_bounds_identify_exact_old_and_new_line_ranges() {
    let fixture = Fixture::new();
    fixture.write("file.txt", b"one\ntwo\nthree\nfour\n");
    fixture.commit_all("base");
    fixture.write("file.txt", b"one\nTWO\ninserted\nthree\nfour\n");
    let repo = Repository::open(fixture.root()).expect("open repository");
    let review = repo
        .review(Base::Index, &[PathBuf::from("file.txt")])
        .expect("inspect changed lines");

    let hunks = review.hunks(Path::new("file.txt"));
    assert_eq!(hunks.len(), 1);
    assert_eq!(
        (
            hunks[0].old_start,
            hunks[0].old_end,
            hunks[0].new_start,
            hunks[0].new_end
        ),
        (1, 2, 1, 3)
    );
}
