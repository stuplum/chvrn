mod support;

use chvrn_git::{Base, GitError, Repository};
use std::path::PathBuf;
use support::Fixture;

const ORIGINAL: &[u8] = b"alpha\r\nold";
const UPDATED: &[u8] = b"alpha\r\nnew";
const PATCH: &[u8] = b"--- a/note.txt\n+++ b/note.txt\n@@ -1,2 +1,2 @@\n alpha\r\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n";

#[test]
fn exported_patch_applies_and_reverses_with_git_preserving_crlf_and_no_final_newline() {
    let fixture = Fixture::new();
    fixture.write("note.txt", ORIGINAL);
    fixture.commit_all("initial");
    fixture.write("note.txt", UPDATED);
    let repo = Repository::open(fixture.root()).expect("open repository");
    let review = repo
        .review(Base::Revision(fixture.head()), &[PathBuf::from("note.txt")])
        .expect("inspect changed file");

    let exported = repo.export_patch(&review).expect("export unified patch");
    let target = Fixture::new();
    target.write("note.txt", ORIGINAL);
    target.commit_all("matching base");

    target.git_with_input(&["apply", "-"], &exported);
    assert_eq!(target.read("note.txt"), UPDATED);
    assert_eq!(target.git(&["show", ":note.txt"]), ORIGINAL);

    target.git_with_input(&["apply", "--reverse", "-"], &exported);
    assert_eq!(target.read("note.txt"), ORIGINAL);
    assert_eq!(fixture.read("note.txt"), UPDATED);
}

#[test]
fn imported_patch_changes_only_the_worktree_and_preserves_exact_line_endings() {
    let fixture = Fixture::new();
    fixture.write("note.txt", ORIGINAL);
    fixture.commit_all("initial");
    let repo = Repository::open(fixture.root()).expect("open repository");
    let review = repo
        .review(Base::Index, &[PathBuf::from("note.txt")])
        .expect("inspect patch preimage");

    repo.import_patch(&review, PATCH)
        .expect("import unified patch");

    assert_eq!(fixture.read("note.txt"), UPDATED);
    assert_eq!(fixture.git(&["show", ":note.txt"]), ORIGINAL);
}

#[test]
fn patch_preview_exposes_candidate_bytes_without_writing_or_staging() {
    let fixture = Fixture::new();
    fixture.write("note.txt", ORIGINAL);
    fixture.commit_all("initial");
    let repo = Repository::open(fixture.root()).expect("open repository");
    let review = repo
        .review(Base::Index, &[PathBuf::from("note.txt")])
        .expect("inspect patch preimage");

    let candidates = repo.preview_patch(&review, PATCH).expect("preview patch");

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].path, PathBuf::from("note.txt"));
    assert_eq!(candidates[0].bytes.as_deref(), Some(UPDATED));
    assert_eq!(fixture.read("note.txt"), ORIGINAL);
    assert_eq!(fixture.git(&["show", ":note.txt"]), ORIGINAL);
}

#[test]
fn patch_review_includes_an_unchanged_target_and_an_absent_addition() {
    let fixture = Fixture::new();
    fixture.write("note.txt", ORIGINAL);
    fixture.commit_all("initial");
    let patch = b"--- a/note.txt\n+++ b/note.txt\n@@ -1,2 +1,2 @@\n alpha\r\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n--- /dev/null\n+++ b/fresh.txt\n@@ -0,0 +1 @@\n+fresh\n";
    let repo = Repository::open(fixture.root()).expect("open repository");

    let review = repo
        .review_patch(Base::Index, patch)
        .expect("inspect patch paths");
    let candidates = repo
        .preview_patch(&review, patch)
        .expect("preview both files");

    assert_eq!(review.files().len(), 2);
    assert_eq!(candidates[0].path, PathBuf::from("fresh.txt"));
    assert_eq!(candidates[0].bytes.as_deref(), Some(b"fresh\n".as_slice()));
    assert_eq!(candidates[1].path, PathBuf::from("note.txt"));
    assert_eq!(candidates[1].bytes.as_deref(), Some(UPDATED));
    assert_eq!(fixture.read("note.txt"), ORIGINAL);
    assert!(!fixture.root().join("fresh.txt").exists());
}

#[cfg(unix)]
#[test]
fn importing_a_new_executable_file_keeps_its_patch_mode_without_staging() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    fixture.write("anchor.txt", b"anchor\n");
    fixture.commit_all("initial");
    let patch = b"diff --git a/launch.sh b/launch.sh\nnew file mode 100755\n--- /dev/null\n+++ b/launch.sh\n@@ -0,0 +1 @@\n+echo ready\n";
    let repo = Repository::open(fixture.root()).expect("open repository");
    let review = repo
        .review_patch(Base::Index, patch)
        .expect("inspect new executable");

    repo.import_patch(&review, patch)
        .expect("import executable patch");

    assert_eq!(fixture.read("launch.sh"), b"echo ready\n");
    assert_ne!(
        std::fs::metadata(fixture.root().join("launch.sh"))
            .expect("new file metadata")
            .permissions()
            .mode()
            & 0o111,
        0
    );
    assert!(fixture.git(&["ls-files", "--", "launch.sh"]).is_empty());
}

#[test]
fn malformed_second_file_cannot_partially_apply_a_valid_first_file() {
    let fixture = Fixture::new();
    fixture.write("first.txt", b"first old\n");
    fixture.write("second.txt", b"second old\n");
    fixture.commit_all("initial");
    let repo = Repository::open(fixture.root()).expect("open repository");
    let review = repo
        .review(
            Base::Index,
            &[PathBuf::from("first.txt"), PathBuf::from("second.txt")],
        )
        .expect("inspect both patch targets");
    let patch = b"--- a/first.txt\n+++ b/first.txt\n@@ -1 +1 @@\n-first old\n+first new\n--- a/second.txt\n+++ b/second.txt\n@@ malformed @@\n-second old\n+second new\n";

    assert_eq!(
        repo.import_patch(&review, patch),
        Err(GitError::MalformedPatch)
    );
    assert_eq!(fixture.read("first.txt"), b"first old\n");
    assert_eq!(fixture.read("second.txt"), b"second old\n");
    assert_eq!(fixture.git(&["show", ":first.txt"]), b"first old\n");
    assert_eq!(fixture.git(&["show", ":second.txt"]), b"second old\n");
}

#[test]
fn patch_preimage_mismatch_does_not_overwrite_either_file() {
    let fixture = Fixture::new();
    fixture.write("first.txt", b"first old\n");
    fixture.write("second.txt", b"second old\n");
    fixture.commit_all("initial");
    let repo = Repository::open(fixture.root()).expect("open repository");
    let review = repo
        .review(
            Base::Index,
            &[PathBuf::from("first.txt"), PathBuf::from("second.txt")],
        )
        .expect("inspect both patch targets");
    let patch = b"--- a/first.txt\n+++ b/first.txt\n@@ -1 +1 @@\n-first old\n+first new\n--- a/second.txt\n+++ b/second.txt\n@@ -1 +1 @@\n-second wrong\n+second new\n";

    assert!(matches!(
        repo.import_patch(&review, patch),
        Err(GitError::MalformedPatch)
    ));
    assert_eq!(fixture.read("first.txt"), b"first old\n");
    assert_eq!(fixture.read("second.txt"), b"second old\n");
}

#[test]
fn patch_paths_cannot_escape_the_reviewed_repository() {
    let fixture = Fixture::new();
    fixture.write("safe.txt", b"safe\n");
    fixture.commit_all("initial");
    let outside = fixture.outside();
    std::fs::write(&outside, b"outside\n").expect("create outside file");
    let repo = Repository::open(fixture.root()).expect("open repository");
    let review = repo
        .review(Base::Index, &[PathBuf::from("safe.txt")])
        .expect("inspect in-root file");
    let patch = b"--- a/../outside.txt\n+++ b/../outside.txt\n@@ -1 +1 @@\n-outside\n+corrupt\n";

    assert!(matches!(
        repo.import_patch(&review, patch),
        Err(GitError::UnsafePath)
    ));
    assert_eq!(
        std::fs::read(outside).expect("read outside file"),
        b"outside\n"
    );
    assert_eq!(fixture.read("safe.txt"), b"safe\n");
}

#[cfg(unix)]
#[test]
fn patch_import_refuses_a_new_symlink_target() {
    let fixture = Fixture::new();
    fixture.write("anchor.txt", b"anchor\n");
    fixture.commit_all("initial");
    let patch = b"diff --git a/link.txt b/link.txt\nnew file mode 120000\n--- /dev/null\n+++ b/link.txt\n@@ -0,0 +1 @@\n+../outside.txt\n\\ No newline at end of file\n";
    let repo = Repository::open(fixture.root()).expect("open repository");
    assert!(matches!(
        repo.review_patch(Base::Index, patch),
        Err(GitError::UnsafePath)
    ));
    let review = repo
        .review(Base::Index, &[PathBuf::from("link.txt")])
        .expect("inspect absent link path");

    assert!(matches!(
        repo.import_patch(&review, patch),
        Err(GitError::UnsafePath)
    ));
    assert!(!fixture.root().join("link.txt").exists());
    assert_eq!(fixture.git(&["show", ":anchor.txt"]), b"anchor\n");
}
