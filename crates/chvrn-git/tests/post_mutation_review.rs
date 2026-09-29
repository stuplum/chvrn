mod support;

use chvrn_git::{Base, GitError, PatchCandidate, Repository};
use std::path::{Path, PathBuf};
use support::Fixture;

fn candidate(path: &str, bytes: Option<&[u8]>) -> PatchCandidate {
    PatchCandidate {
        path: PathBuf::from(path),
        bytes: bytes.map(ToOwned::to_owned),
        mode: bytes.map(|_| 0o100644),
    }
}

#[test]
fn authorised_edit_returns_a_review_of_all_original_paths() {
    let fixture = Fixture::new();
    fixture.write("edited.txt", b"before\n");
    fixture.write("other.txt", b"other\n");
    fixture.commit_all("initial");
    let repo = Repository::open(fixture.root()).expect("open repository");
    let before = repo
        .review(
            Base::Revision(fixture.head()),
            &[PathBuf::from("edited.txt"), PathBuf::from("other.txt")],
        )
        .expect("inspect both files");
    repo.save_worktree(&before, Path::new("edited.txt"), b"after\r\n")
        .expect("authorised edit");

    let after = repo
        .review_after_changes(&before, &[candidate("edited.txt", Some(b"after\r\n"))])
        .expect("reinspect authorised result");

    assert_eq!(after.files().len(), 2);
    assert_eq!(
        after
            .file(Path::new("edited.txt"))
            .expect("edited path")
            .worktree
            .as_deref(),
        Some(b"after\r\n".as_slice())
    );
    assert_eq!(
        after
            .file(Path::new("other.txt"))
            .expect("unchanged path")
            .worktree
            .as_deref(),
        Some(b"other\n".as_slice())
    );
    assert_eq!(fixture.git(&["show", ":edited.txt"]), b"before\n");
    repo.validate_review(&after)
        .expect("new review remains current");
}

#[test]
fn authorised_deletion_keeps_the_now_absent_path_in_the_review() {
    let fixture = Fixture::new();
    fixture.write("other.txt", b"other\n");
    fixture.commit_all("initial");
    fixture.write("remove.txt", b"untracked content\n");
    let repo = Repository::open(fixture.root()).expect("open repository");
    let before = repo
        .review(
            Base::Revision(fixture.head()),
            &[PathBuf::from("remove.txt"), PathBuf::from("other.txt")],
        )
        .expect("inspect addition and unchanged path");
    repo.reject_file(&before, Path::new("remove.txt"))
        .expect("authorised deletion");

    let after = repo
        .review_after_changes(&before, &[candidate("remove.txt", None)])
        .expect("reinspect deletion");

    assert_eq!(after.files().len(), 2);
    assert!(
        after
            .file(Path::new("remove.txt"))
            .expect("deleted path retained")
            .worktree
            .is_none()
    );
    assert_eq!(
        after
            .file(Path::new("other.txt"))
            .expect("other path retained")
            .worktree
            .as_deref(),
        Some(b"other\n".as_slice())
    );
    assert!(!fixture.root().join("remove.txt").exists());
    repo.validate_review(&after)
        .expect("new review remains current");
}

#[test]
fn authorised_new_path_is_added_without_dropping_an_inspected_path() {
    let fixture = Fixture::new();
    fixture.write("inspected.txt", b"unchanged\n");
    fixture.commit_all("initial");
    let repo = Repository::open(fixture.root()).expect("open repository");
    let before = repo
        .review(Base::Index, &[PathBuf::from("inspected.txt")])
        .expect("inspect existing path");
    fixture.write("new.txt", b"new\n");

    let after = repo
        .review_after_changes(&before, &[candidate("new.txt", Some(b"new\n"))])
        .expect("reinspect authorised new path");

    assert_eq!(after.files().len(), 2);
    assert_eq!(
        after
            .file(Path::new("new.txt"))
            .expect("new path")
            .worktree
            .as_deref(),
        Some(b"new\n".as_slice())
    );
    assert_eq!(
        after
            .file(Path::new("inspected.txt"))
            .expect("original path")
            .worktree
            .as_deref(),
        Some(b"unchanged\n".as_slice())
    );
    repo.validate_review(&after)
        .expect("new review remains current");
}

#[test]
fn changed_file_must_match_the_authorised_post_bytes() {
    let fixture = Fixture::new();
    fixture.write("file.txt", b"before\n");
    fixture.commit_all("initial");
    let repo = Repository::open(fixture.root()).expect("open repository");
    let before = repo
        .review(Base::Index, &[PathBuf::from("file.txt")])
        .expect("inspect original");
    fixture.write("file.txt", b"different result\n");

    assert!(matches!(
        repo.review_after_changes(
            &before,
            &[candidate("file.txt", Some(b"expected result\n"))]
        ),
        Err(GitError::StaleReview)
    ));
    assert_eq!(fixture.read("file.txt"), b"different result\n");
}

#[cfg(unix)]
#[test]
fn changed_file_must_match_the_authorised_post_mode() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    fixture.write("file.txt", b"before\n");
    fixture.commit_all("initial");
    let repo = Repository::open(fixture.root()).expect("open repository");
    let before = repo
        .review(Base::Index, &[PathBuf::from("file.txt")])
        .expect("inspect original");
    fixture.write("file.txt", b"expected result\n");
    std::fs::set_permissions(
        fixture.root().join("file.txt"),
        std::fs::Permissions::from_mode(0o755),
    )
    .expect("mark executable");

    assert!(matches!(
        repo.review_after_changes(
            &before,
            &[candidate("file.txt", Some(b"expected result\n"))]
        ),
        Err(GitError::StaleReview)
    ));
    assert_ne!(
        std::fs::metadata(fixture.root().join("file.txt"))
            .expect("result metadata")
            .permissions()
            .mode()
            & 0o111,
        0
    );
}

#[test]
fn concurrent_change_to_another_inspected_file_is_not_approved() {
    let fixture = Fixture::new();
    fixture.write("edited.txt", b"before\n");
    fixture.write("other.txt", b"other\n");
    fixture.commit_all("initial");
    let repo = Repository::open(fixture.root()).expect("open repository");
    let before = repo
        .review(
            Base::Index,
            &[PathBuf::from("edited.txt"), PathBuf::from("other.txt")],
        )
        .expect("inspect both paths");
    repo.save_worktree(&before, Path::new("edited.txt"), b"authorised\n")
        .expect("authorised edit");
    fixture.write("other.txt", b"concurrent\n");

    assert!(matches!(
        repo.review_after_changes(&before, &[candidate("edited.txt", Some(b"authorised\n"))]),
        Err(GitError::StaleReview)
    ));
    assert_eq!(fixture.read("edited.txt"), b"authorised\n");
    assert_eq!(fixture.read("other.txt"), b"concurrent\n");
}

#[test]
fn concurrent_index_only_change_is_not_folded_into_the_new_review() {
    let fixture = Fixture::new();
    fixture.write("edited.txt", b"before\n");
    fixture.write("other.txt", b"other\n");
    fixture.commit_all("initial");
    let repo = Repository::open(fixture.root()).expect("open repository");
    let before = repo
        .review(
            Base::Index,
            &[PathBuf::from("edited.txt"), PathBuf::from("other.txt")],
        )
        .expect("inspect both paths");
    repo.save_worktree(&before, Path::new("edited.txt"), b"authorised\n")
        .expect("authorised edit");
    fixture.write("other.txt", b"concurrent index\n");
    fixture.git(&["add", "--", "other.txt"]);
    fixture.write("other.txt", b"other\n");

    assert!(matches!(
        repo.review_after_changes(&before, &[candidate("edited.txt", Some(b"authorised\n"))]),
        Err(GitError::StaleReview)
    ));
    assert_eq!(fixture.git(&["show", ":other.txt"]), b"concurrent index\n");
    assert_eq!(fixture.read("other.txt"), b"other\n");
}

#[test]
fn moved_revision_is_not_accepted_as_the_original_base() {
    let fixture = Fixture::new();
    fixture.write("file.txt", b"old tree\n");
    fixture.commit_all("old");
    let old_head = fixture.head();
    fixture.write("file.txt", b"new tree\n");
    fixture.commit_all("new");
    let repo = Repository::open(fixture.root()).expect("open repository");
    let before = repo
        .review(Base::Revision("HEAD".into()), &[PathBuf::from("file.txt")])
        .expect("inspect new tree");
    fixture.git(&["update-ref", "HEAD", &old_head]);
    assert!(matches!(
        repo.validate_review(&before),
        Err(GitError::StaleReview)
    ));

    assert!(matches!(
        repo.review_after_changes(&before, &[]),
        Err(GitError::StaleReview)
    ));
    assert_eq!(fixture.read("file.txt"), b"new tree\n");
}

#[test]
fn immutable_revision_remains_valid_when_head_moves() {
    let fixture = Fixture::new();
    fixture.write("file.txt", b"old tree\n");
    fixture.commit_all("old");
    let old_head = fixture.head();
    fixture.write("file.txt", b"new tree\n");
    fixture.commit_all("new");
    let immutable_base = fixture.head();
    let repo = Repository::open(fixture.root()).expect("open repository");
    let before = repo
        .review(Base::Revision(immutable_base), &[PathBuf::from("file.txt")])
        .expect("inspect immutable revision");
    fixture.git(&["update-ref", "HEAD", &old_head]);

    repo.validate_review(&before)
        .expect("immutable base still resolves");
    let after = repo
        .review_after_changes(&before, &[])
        .expect("reinspect unchanged file");
    assert_eq!(
        after
            .file(Path::new("file.txt"))
            .expect("reviewed path")
            .worktree
            .as_deref(),
        Some(b"new tree\n".as_slice())
    );
}
