mod support;

use chvrn_git::{Base, GitError, Repository};
use std::path::{Path, PathBuf};
use support::Fixture;

const ORIGINAL: &str = "01\n02\n03\n04\n05\n06\n07\n08\n09\n10\n11\n12\n13\n14\n15\n16\n17\n18\n19\n20\n21\n22\n23\n24\n";
const STAGED: &str = "first staged\n02\n03\n04\n05\n06\n07\n08\n09\n10\n11\n12\n13\n14\n15\n16\n17\n18\n19\n20\n21\n22\n23\n24\n";
const WORKTREE: &str = "first staged\n02\n03\n04\n05\n06\n07\n08\n09\n10\nselected\n12\n13\n14\n15\n16\n17\n18\n19\n20\nlater unstaged\n22\n23\n24\n";
const INDEX_AFTER_STAGE: &str = "first staged\n02\n03\n04\n05\n06\n07\n08\n09\n10\nselected\n12\n13\n14\n15\n16\n17\n18\n19\n20\n21\n22\n23\n24\n";

#[test]
fn staging_one_hunk_preserves_existing_index_content_and_other_worktree_edits() {
    let fixture = Fixture::new();
    fixture.write("main.txt", ORIGINAL.as_bytes());
    fixture.write("other.txt", b"other original\n");
    fixture.commit_all("initial");
    fixture.write("main.txt", STAGED.as_bytes());
    fixture.git(&["add", "--", "main.txt"]);
    fixture.write("main.txt", WORKTREE.as_bytes());
    fixture.write("other.txt", b"other unstaged\n");

    let repo = Repository::open(fixture.root()).expect("open repository");
    let review = repo
        .review(Base::Index, &[PathBuf::from("main.txt")])
        .expect("inspect index and worktree");
    let chosen = review
        .hunks(Path::new("main.txt"))
        .iter()
        .find(|hunk| hunk.new_start == 10)
        .expect("line eleven is selectable")
        .id;
    repo.stage(&review, &[chosen]).expect("stage chosen hunk");

    assert_eq!(
        fixture.git(&["show", ":main.txt"]),
        INDEX_AFTER_STAGE.as_bytes()
    );
    assert_eq!(fixture.git(&["show", ":other.txt"]), b"other original\n");
    assert_eq!(fixture.read("main.txt"), WORKTREE.as_bytes());
    assert_eq!(fixture.read("other.txt"), b"other unstaged\n");
}

#[test]
fn rejecting_a_hunk_uses_the_pinned_non_head_base_without_touching_other_changes() {
    let fixture = Fixture::new();
    fixture.write("main.txt", ORIGINAL.as_bytes());
    fixture.write("other.txt", b"other original\n");
    fixture.commit_all("base");
    let base = fixture.head();
    fixture.write("main.txt", STAGED.as_bytes());
    fixture.commit_all("new head");
    fixture.write("main.txt", WORKTREE.as_bytes());
    fixture.write("other.txt", b"other staged\n");
    fixture.git(&["add", "--", "other.txt"]);
    fixture.write("other.txt", b"other unstaged too\n");

    let repo = Repository::open(fixture.root()).expect("open repository");
    let review = repo
        .review(Base::Revision(base), &[PathBuf::from("main.txt")])
        .expect("inspect explicit base");
    let chosen = review
        .hunks(Path::new("main.txt"))
        .iter()
        .find(|hunk| hunk.new_start == 0)
        .expect("first-line difference exists")
        .id;
    repo.reject(&review, &[chosen])
        .expect("reject first-line difference");

    assert_eq!(fixture.read("main.txt"), b"01\n02\n03\n04\n05\n06\n07\n08\n09\n10\nselected\n12\n13\n14\n15\n16\n17\n18\n19\n20\nlater unstaged\n22\n23\n24\n");
    assert_eq!(fixture.git(&["show", ":main.txt"]), STAGED.as_bytes());
    assert_eq!(fixture.git(&["show", ":other.txt"]), b"other staged\n");
    assert_eq!(fixture.read("other.txt"), b"other unstaged too\n");
}

#[test]
fn stale_multifile_stage_rejects_every_requested_write() {
    let fixture = Fixture::new();
    fixture.write("a.txt", b"old a\n");
    fixture.write("b.txt", b"old b\n");
    fixture.commit_all("initial");
    fixture.write("a.txt", b"selected a\n");
    fixture.write("b.txt", b"selected b\n");

    let repo = Repository::open(fixture.root()).expect("open repository");
    let review = repo
        .review(
            Base::Index,
            &[PathBuf::from("a.txt"), PathBuf::from("b.txt")],
        )
        .expect("inspect both paths");
    let a = review.hunks(Path::new("a.txt"))[0].id;
    let b = review.hunks(Path::new("b.txt"))[0].id;
    assert_ne!(a, b, "hunks in different files need distinct IDs");
    fixture.write("b.txt", b"concurrent b\n");

    assert_eq!(repo.stage(&review, &[a, b]), Err(GitError::StaleReview));
    assert_eq!(fixture.git(&["show", ":a.txt"]), b"old a\n");
    assert_eq!(fixture.git(&["show", ":b.txt"]), b"old b\n");
    assert_eq!(fixture.read("a.txt"), b"selected a\n");
    assert_eq!(fixture.read("b.txt"), b"concurrent b\n");
}

#[test]
fn concurrent_index_update_prevents_staging_any_reviewed_file() {
    let fixture = Fixture::new();
    fixture.write("a.txt", b"old a\n");
    fixture.write("b.txt", b"old b\n");
    fixture.commit_all("initial");
    fixture.write("a.txt", b"selected a\n");
    fixture.write("b.txt", b"selected b\n");

    let repo = Repository::open(fixture.root()).expect("open repository");
    let review = repo
        .review(
            Base::Index,
            &[PathBuf::from("a.txt"), PathBuf::from("b.txt")],
        )
        .expect("inspect both paths");
    let a = review.hunks(Path::new("a.txt"))[0].id;
    let b = review.hunks(Path::new("b.txt"))[0].id;
    fixture.write("b.txt", b"concurrent staged b\n");
    fixture.git(&["add", "--", "b.txt"]);
    fixture.write("b.txt", b"selected b\n");

    assert_eq!(repo.stage(&review, &[a, b]), Err(GitError::StaleReview));
    assert_eq!(fixture.git(&["show", ":a.txt"]), b"old a\n");
    assert_eq!(fixture.git(&["show", ":b.txt"]), b"concurrent staged b\n");
    assert_eq!(fixture.read("a.txt"), b"selected a\n");
    assert_eq!(fixture.read("b.txt"), b"selected b\n");
}

#[test]
fn stale_rejection_does_not_overwrite_a_concurrent_worktree_edit() {
    let fixture = Fixture::new();
    fixture.write("note.txt", b"base\n");
    fixture.commit_all("base");
    fixture.write("note.txt", b"reviewed\n");

    let repo = Repository::open(fixture.root()).expect("open repository");
    let review = repo
        .review(Base::Revision(fixture.head()), &[PathBuf::from("note.txt")])
        .expect("inspect worktree");
    let chosen = review.hunks(Path::new("note.txt"))[0].id;
    fixture.write("note.txt", b"concurrent\n");

    assert!(matches!(
        repo.reject(&review, &[chosen]),
        Err(GitError::StaleReview)
    ));
    assert_eq!(fixture.read("note.txt"), b"concurrent\n");
    assert_eq!(fixture.git(&["show", ":note.txt"]), b"base\n");
}

#[test]
fn staging_with_a_revision_based_review_is_rejected_without_mutation() {
    let fixture = Fixture::new();
    fixture.write("note.txt", b"base\n");
    fixture.commit_all("base");
    fixture.write("note.txt", b"reviewed\n");
    let repo = Repository::open(fixture.root()).expect("open repository");
    let review = repo
        .review(Base::Revision(fixture.head()), &[PathBuf::from("note.txt")])
        .expect("inspect revision");
    let hunk = review.hunks(Path::new("note.txt"))[0].id;

    assert!(matches!(
        repo.stage(&review, &[hunk]),
        Err(GitError::InvalidBase)
    ));
    assert_eq!(fixture.git(&["show", ":note.txt"]), b"base\n");
    assert_eq!(fixture.read("note.txt"), b"reviewed\n");
}

#[test]
fn a_hunk_id_from_another_review_cannot_stage_content() {
    let fixture = Fixture::new();
    fixture.write("note.txt", b"base\n");
    fixture.commit_all("base");
    fixture.write("note.txt", b"reviewed\n");
    let repo = Repository::open(fixture.root()).expect("open repository");
    let first = repo
        .review(Base::Index, &[PathBuf::from("note.txt")])
        .expect("first review");
    let second = repo
        .review(Base::Index, &[PathBuf::from("note.txt")])
        .expect("second review");
    let foreign = first.hunks(Path::new("note.txt"))[0].id;

    assert!(matches!(
        repo.stage(&second, &[foreign]),
        Err(GitError::ForeignHunk)
    ));
    assert_eq!(fixture.git(&["show", ":note.txt"]), b"base\n");
    assert_eq!(fixture.read("note.txt"), b"reviewed\n");
}

#[cfg(unix)]
#[test]
fn rejecting_text_preserves_the_executable_bit() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    fixture.write("run.sh", b"#!/bin/sh\nprintf original\n");
    std::fs::set_permissions(
        fixture.root().join("run.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .expect("mark executable");
    fixture.commit_all("executable base");
    fixture.write("run.sh", b"#!/bin/sh\nprintf changed\n");

    let repo = Repository::open(fixture.root()).expect("open repository");
    let review = repo
        .review(Base::Revision(fixture.head()), &[PathBuf::from("run.sh")])
        .expect("inspect executable");
    repo.reject(&review, &[review.hunks(Path::new("run.sh"))[0].id])
        .expect("restore text");

    assert_eq!(fixture.read("run.sh"), b"#!/bin/sh\nprintf original\n");
    assert_ne!(
        std::fs::metadata(fixture.root().join("run.sh"))
            .expect("executable metadata")
            .permissions()
            .mode()
            & 0o111,
        0
    );
    assert!(
        fixture
            .git(&["ls-files", "--stage", "--", "run.sh"])
            .starts_with(b"100755 ")
    );
}

#[cfg(unix)]
#[test]
fn staging_a_mode_only_change_updates_the_index_mode_without_changing_bytes() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    fixture.write("run.sh", b"#!/bin/sh\n");
    fixture.commit_all("initial");
    std::fs::set_permissions(
        fixture.root().join("run.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .expect("mark executable");
    let repo = Repository::open(fixture.root()).expect("open repository");
    let review = repo
        .review(Base::Index, &[PathBuf::from("run.sh")])
        .expect("inspect mode change");

    repo.stage_file(&review, Path::new("run.sh"))
        .expect("stage executable mode");

    assert!(
        fixture
            .git(&["ls-files", "--stage", "--", "run.sh"])
            .starts_with(b"100755 ")
    );
    assert_eq!(fixture.git(&["show", ":run.sh"]), b"#!/bin/sh\n");
    assert_eq!(fixture.read("run.sh"), b"#!/bin/sh\n");
}

#[cfg(unix)]
#[test]
fn rejecting_a_mode_only_change_restores_the_revision_mode() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    fixture.write("run.sh", b"#!/bin/sh\n");
    fixture.commit_all("initial");
    std::fs::set_permissions(
        fixture.root().join("run.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .expect("mark executable");
    let repo = Repository::open(fixture.root()).expect("open repository");
    let review = repo
        .review(Base::Revision(fixture.head()), &[PathBuf::from("run.sh")])
        .expect("inspect mode change");

    repo.reject_file(&review, Path::new("run.sh"))
        .expect("restore original mode");

    assert_eq!(
        std::fs::metadata(fixture.root().join("run.sh"))
            .expect("restored metadata")
            .permissions()
            .mode()
            & 0o111,
        0
    );
    assert!(
        fixture
            .git(&["ls-files", "--stage", "--", "run.sh"])
            .starts_with(b"100644 ")
    );
    assert_eq!(fixture.read("run.sh"), b"#!/bin/sh\n");
}

#[test]
fn staging_an_untracked_file_addition_leaves_another_untracked_file_alone() {
    let fixture = Fixture::new();
    fixture.write("anchor.txt", b"anchor\n");
    fixture.commit_all("initial");
    fixture.write("new file.txt", b"new contents\n");
    fixture.write("other untracked.txt", b"leave alone\n");

    let repo = Repository::open(fixture.root()).expect("open repository");
    let review = repo
        .review(Base::Index, &[PathBuf::from("new file.txt")])
        .expect("inspect untracked addition");
    let addition = review.hunks(Path::new("new file.txt"))[0].id;
    repo.stage(&review, &[addition])
        .expect("stage new file hunk");

    assert_eq!(fixture.git(&["show", ":new file.txt"]), b"new contents\n");
    assert_eq!(fixture.read("new file.txt"), b"new contents\n");
    assert_eq!(fixture.read("other untracked.txt"), b"leave alone\n");
    assert!(
        fixture
            .git(&["ls-files", "--", "other untracked.txt"])
            .is_empty()
    );
}

#[test]
fn rejecting_a_deletion_restores_only_the_reviewed_file() {
    let fixture = Fixture::new();
    fixture.write("removed.txt", b"restore exactly\n");
    fixture.write("other.txt", b"original\n");
    fixture.commit_all("initial");
    std::fs::remove_file(fixture.root().join("removed.txt")).expect("remove reviewed file");
    fixture.write("other.txt", b"keep this change\n");

    let repo = Repository::open(fixture.root()).expect("open repository");
    let review = repo
        .review(
            Base::Revision(fixture.head()),
            &[PathBuf::from("removed.txt")],
        )
        .expect("inspect deletion");
    let deletion = review.hunks(Path::new("removed.txt"))[0].id;
    repo.reject(&review, &[deletion]).expect("reject deletion");

    assert_eq!(fixture.read("removed.txt"), b"restore exactly\n");
    assert_eq!(fixture.read("other.txt"), b"keep this change\n");
    assert_eq!(fixture.git(&["show", ":removed.txt"]), b"restore exactly\n");
}
