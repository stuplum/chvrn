mod support;

use chvrn_git::{Base, GitError, Repository};
use std::fs;
use std::path::Path;
use std::process::Command;
use support::Fixture;

fn merge_conflict(fixture: &Fixture) {
    let output = Command::new("git")
        .current_dir(fixture.root())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").expect("PATH"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "merge",
            "--no-edit",
            "incoming",
        ])
        .output()
        .expect("merge branches");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
}

fn conflict_fixture(base: Option<&[u8]>, ours: Option<&[u8]>, theirs: Option<&[u8]>) -> Fixture {
    let fixture = Fixture::new();
    fixture.write("unchanged.txt", b"ordinary\n");
    if let Some(base) = base {
        fixture.write("conflict.txt", base);
    }
    fixture.commit_all("base");
    fixture.git(&["branch", "incoming"]);
    if let Some(ours) = ours {
        fixture.write("conflict.txt", ours);
    } else {
        fixture.git(&["rm", "--", "conflict.txt"]);
    }
    fixture.commit_all("ours");
    fixture.git(&["checkout", "incoming"]);
    if let Some(theirs) = theirs {
        fixture.write("conflict.txt", theirs);
    } else {
        fixture.git(&["rm", "--", "conflict.txt"]);
    }
    fixture.commit_all("theirs");
    fixture.git(&["checkout", "-"]);
    merge_conflict(&fixture);
    fixture
}

fn text_conflict() -> Fixture {
    conflict_fixture(Some(b"base\r\n"), Some(b"ours\r\n"), Some(b"theirs\r\n"))
}

#[test]
fn unresolved_merge_exposes_exact_stage_inputs_without_touching_index_or_worktree() {
    let fixture = text_conflict();
    fixture.write("staged.txt", b"unrelated staged data\n");
    fixture.git(&["add", "--", "staged.txt"]);
    let index = fs::read(fixture.root().join(".git/index")).expect("index bytes");
    let worktree = fixture.read("conflict.txt");
    let repo = Repository::open(fixture.root()).expect("open");
    let conflict = repo
        .conflict(Path::new("conflict.txt"))
        .expect("inspect")
        .expect("conflict");

    assert_eq!(conflict.path(), Path::new("conflict.txt"));
    assert_eq!(conflict.base(), b"base\r\n");
    assert_eq!(conflict.ours(), b"ours\r\n");
    assert_eq!(conflict.theirs(), b"theirs\r\n");
    repo.validate_conflict(&conflict)
        .expect("unchanged conflict");
    assert_eq!(
        fs::read(fixture.root().join(".git/index")).expect("index"),
        index
    );
    assert_eq!(fixture.read("conflict.txt"), worktree);
    assert_eq!(
        fixture.git(&["show", ":staged.txt"]),
        b"unrelated staged data\n"
    );
}

#[test]
fn both_review_bases_include_unresolved_paths_without_a_stage_zero_entry() {
    let fixture = text_conflict();
    let repo = Repository::open(fixture.root()).expect("open");
    let worktree = fixture.read("conflict.txt");
    for base in [Base::Index, Base::Revision("HEAD".into())] {
        let review = repo.review(base, &[]).expect("review conflicted merge");
        let file = review
            .file(Path::new("conflict.txt"))
            .expect("unresolved file");
        assert_eq!(file.index, None);
        assert_eq!(file.worktree.as_deref(), Some(worktree.as_slice()));
        repo.validate_review(&review)
            .expect("valid conflicted review");
    }
}

#[test]
fn revision_review_retains_conflict_when_worktree_matches_head() {
    let fixture = text_conflict();
    fixture.write("conflict.txt", b"ours\r\n");
    let repo = Repository::open(fixture.root()).expect("open");
    let review = repo
        .review(Base::Revision("HEAD".into()), &[])
        .expect("review");
    let file = review
        .file(Path::new("conflict.txt"))
        .expect("unresolved path");
    assert_eq!(file.index, None);
    assert_eq!(file.worktree.as_deref(), Some(b"ours\r\n".as_slice()));
}

#[test]
fn add_add_conflict_has_empty_base_and_keeps_both_added_versions() {
    let fixture = conflict_fixture(None, Some(b"ours\n"), Some(b"theirs\n"));
    let repo = Repository::open(fixture.root()).expect("open");
    let conflict = repo
        .conflict(Path::new("conflict.txt"))
        .expect("inspect")
        .expect("conflict");
    assert_eq!(conflict.base(), b"");
    assert_eq!(conflict.ours(), b"ours\n");
    assert_eq!(conflict.theirs(), b"theirs\n");
}

#[test]
fn ordinary_and_untracked_files_are_not_conflicts() {
    let fixture = text_conflict();
    fixture.write("untracked.txt", b"new\n");
    let repo = Repository::open(fixture.root()).expect("open");
    for path in ["unchanged.txt", "untracked.txt", "absent.txt"] {
        assert!(
            repo.conflict(Path::new(path))
                .expect("inspect path")
                .is_none()
        );
    }
}

#[test]
fn validation_rejects_changed_worktree_without_mutating_index() {
    let fixture = text_conflict();
    let repo = Repository::open(fixture.root()).expect("open");
    let conflict = repo
        .conflict(Path::new("conflict.txt"))
        .expect("inspect")
        .expect("conflict");
    let index = fs::read(fixture.root().join(".git/index")).expect("index");
    fixture.write("conflict.txt", b"concurrent edit\n");
    assert_eq!(
        repo.validate_conflict(&conflict),
        Err(GitError::StaleConflict)
    );
    assert_eq!(fixture.read("conflict.txt"), b"concurrent edit\n");
    assert_eq!(
        fs::read(fixture.root().join(".git/index")).expect("index"),
        index
    );
}

#[test]
fn validation_rejects_resolved_conflict_even_with_unchanged_worktree() {
    let fixture = text_conflict();
    let repo = Repository::open(fixture.root()).expect("open");
    let conflict = repo
        .conflict(Path::new("conflict.txt"))
        .expect("inspect")
        .expect("conflict");
    fixture.git(&["add", "--", "conflict.txt"]);
    assert_eq!(
        repo.validate_conflict(&conflict),
        Err(GitError::StaleConflict)
    );
}

#[test]
fn validation_rejects_changed_stage_object_and_mode() {
    for mode_only in [false, true] {
        let fixture = text_conflict();
        let repo = Repository::open(fixture.root()).expect("open");
        let conflict = repo
            .conflict(Path::new("conflict.txt"))
            .expect("inspect")
            .expect("conflict");
        let oid = if mode_only {
            fixture.git(&["rev-parse", ":2:conflict.txt"])
        } else {
            fixture.git_with_input(&["hash-object", "-w", "--stdin"], b"different ours\n")
        };
        let oid = String::from_utf8(oid).expect("object id");
        let mode = if mode_only { "100755" } else { "100644" };
        fixture.git_with_input(
            &["update-index", "--index-info"],
            format!("{mode} {} 2\tconflict.txt\n", oid.trim()).as_bytes(),
        );
        assert_eq!(
            repo.validate_conflict(&conflict),
            Err(GitError::StaleConflict)
        );
    }
}

#[test]
fn unrelated_index_changes_do_not_invalidate_selected_conflict() {
    let fixture = text_conflict();
    let repo = Repository::open(fixture.root()).expect("open");
    let conflict = repo
        .conflict(Path::new("conflict.txt"))
        .expect("inspect")
        .expect("conflict");
    fixture.write("unchanged.txt", b"new unrelated staged contents\n");
    fixture.git(&["add", "--", "unchanged.txt"]);
    repo.validate_conflict(&conflict)
        .expect("only selected stages are guarded");
    assert_eq!(
        fixture.git(&["show", ":unchanged.txt"]),
        b"new unrelated staged contents\n"
    );
}

#[test]
fn snapshot_from_another_repository_is_refused() {
    let first = text_conflict();
    let second = text_conflict();
    let first_repo = Repository::open(first.root()).expect("first repo");
    let second_repo = Repository::open(second.root()).expect("second repo");
    let conflict = first_repo
        .conflict(Path::new("conflict.txt"))
        .expect("inspect")
        .expect("conflict");
    assert_eq!(
        second_repo.validate_conflict(&conflict),
        Err(GitError::ForeignConflict)
    );
}

#[test]
fn modify_delete_conflicts_report_the_missing_side() {
    for (ours, theirs, stage) in [
        (Some(b"ours\n".as_slice()), None, 3),
        (None, Some(b"theirs\n".as_slice()), 2),
    ] {
        let fixture = conflict_fixture(Some(b"base\n"), ours, theirs);
        let repo = Repository::open(fixture.root()).expect("open");
        assert!(
            matches!(repo.conflict(Path::new("conflict.txt")), Err(GitError::MissingConflictSide { stage: actual }) if actual == stage)
        );
    }
}

#[test]
fn binary_conflict_sources_are_refused_even_when_worktree_is_text() {
    for (base, ours, theirs) in [
        (
            b"base\0\n".as_slice(),
            b"ours\n".as_slice(),
            b"theirs\n".as_slice(),
        ),
        (
            b"base\n".as_slice(),
            b"ours\0\n".as_slice(),
            b"theirs\n".as_slice(),
        ),
        (
            b"base\n".as_slice(),
            b"ours\n".as_slice(),
            b"theirs\xff\n".as_slice(),
        ),
    ] {
        let fixture = conflict_fixture(Some(base), Some(ours), Some(theirs));
        fixture.write("conflict.txt", b"text worktree\n");
        let repo = Repository::open(fixture.root()).expect("open");
        assert!(matches!(
            repo.conflict(Path::new("conflict.txt")),
            Err(GitError::BinaryContent)
        ));
    }
}

#[test]
fn binary_worktree_is_not_treated_as_a_text_merge() {
    let fixture = text_conflict();
    fixture.write("conflict.txt", b"binary\0worktree");
    let repo = Repository::open(fixture.root()).expect("open");
    assert!(matches!(
        repo.conflict(Path::new("conflict.txt")),
        Err(GitError::BinaryContent)
    ));
}

#[test]
fn symlink_and_gitlink_stages_are_refused_without_loading_as_blobs() {
    for (mode, stage) in [("120000", 1), ("120000", 2), ("160000", 3)] {
        let fixture = text_conflict();
        let oid = if mode == "160000" {
            fixture.head()
        } else {
            String::from_utf8(fixture.git_with_input(&["hash-object", "-w", "--stdin"], b"target"))
                .expect("oid")
                .trim()
                .to_owned()
        };
        fixture.git_with_input(
            &["update-index", "--index-info"],
            format!("{mode} {oid} {stage}\tconflict.txt\n").as_bytes(),
        );
        let repo = Repository::open(fixture.root()).expect("open");
        assert!(
            matches!(repo.conflict(Path::new("conflict.txt")), Err(GitError::NonRegularConflict { stage: actual }) if actual == stage)
        );
    }
}

#[test]
fn traversal_and_git_metadata_paths_are_refused() {
    let fixture = text_conflict();
    let repo = Repository::open(fixture.root()).expect("open");
    for path in ["../outside.txt", ".git/index", "/tmp/conflict.txt"] {
        assert!(matches!(
            repo.conflict(Path::new(path)),
            Err(GitError::UnsafePath)
        ));
    }
}

#[test]
fn literal_pathspec_characters_do_not_select_another_conflict() {
    let fixture = text_conflict();
    let repo = Repository::open(fixture.root()).expect("open");
    assert!(
        repo.conflict(Path::new("*.txt"))
            .expect("literal filename")
            .is_none()
    );
    assert!(
        repo.conflict(Path::new(":(glob)*"))
            .expect("literal magic")
            .is_none()
    );
}

#[cfg(unix)]
#[test]
fn worktree_permission_changes_invalidate_conflict_even_without_executable_bit_change() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = text_conflict();
    let path = fixture.root().join("conflict.txt");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("initial mode");
    let repo = Repository::open(fixture.root()).expect("open");
    let conflict = repo
        .conflict(Path::new("conflict.txt"))
        .expect("inspect")
        .expect("conflict");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("change mode");
    assert_eq!(
        repo.validate_conflict(&conflict),
        Err(GitError::StaleConflict)
    );
}

#[cfg(unix)]
#[test]
fn replaced_worktree_symlink_is_refused() {
    use std::os::unix::fs::symlink;
    let fixture = text_conflict();
    let repo = Repository::open(fixture.root()).expect("open");
    let conflict = repo
        .conflict(Path::new("conflict.txt"))
        .expect("inspect")
        .expect("conflict");
    fs::remove_file(fixture.root().join("conflict.txt")).expect("remove worktree");
    symlink("unchanged.txt", fixture.root().join("conflict.txt")).expect("replace with symlink");
    assert_eq!(repo.validate_conflict(&conflict), Err(GitError::UnsafePath));
    assert!(matches!(
        repo.conflict(Path::new("conflict.txt")),
        Err(GitError::UnsafePath)
    ));
    assert_eq!(fixture.read("unchanged.txt"), b"ordinary\n");
}

#[test]
fn replaced_worktree_directory_is_refused() {
    let fixture = text_conflict();
    let repo = Repository::open(fixture.root()).expect("open");
    let conflict = repo
        .conflict(Path::new("conflict.txt"))
        .expect("inspect")
        .expect("conflict");
    fs::remove_file(fixture.root().join("conflict.txt")).expect("remove worktree");
    fs::create_dir(fixture.root().join("conflict.txt")).expect("replace with directory");
    assert_eq!(
        repo.validate_conflict(&conflict),
        Err(GitError::NonRegularConflict { stage: 0 })
    );
}
