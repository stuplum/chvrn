use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

fn invoke(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_chvrn"))
        .args(args)
        .current_dir(root)
        .env_remove("HERDR_ENV")
        .env_remove("HERDR_PANE_ID")
        .env_remove("HERDR_WORKSPACE_ID")
        .env_remove("HERDR_TAB_ID")
        .output()
        .expect("run chvrn")
}

fn files(entries: &[(&str, &[u8])]) -> TempDir {
    let root = tempfile::tempdir().expect("isolated CLI fixture");
    for (name, bytes) in entries {
        fs::write(root.path().join(name), bytes).expect("write CLI fixture");
    }
    root
}

#[test]
fn redirected_diff_returns_machine_readable_changes_without_mutating_inputs() {
    let root = files(&[("left.txt", b"head\r\nold"), ("right.txt", b"head\r\nnew")]);
    let output = invoke(
        root.path(),
        &["diff", "left.txt", "right.txt", "--format", "json"],
    );

    assert_eq!(output.status.code(), Some(1));
    let result: Value = serde_json::from_slice(&output.stdout).expect("headless JSON");
    assert_eq!(result["equal"], false);
    assert_eq!(
        result["hunks"][0]["left"],
        serde_json::json!({"start": 1, "end": 2})
    );
    assert_eq!(
        result["hunks"][0]["right"],
        serde_json::json!({"start": 1, "end": 2})
    );
    assert_eq!(result["hunks"].as_array().unwrap().len(), 1);
    assert_eq!(
        fs::read(root.path().join("left.txt")).unwrap(),
        b"head\r\nold"
    );
    assert_eq!(
        fs::read(root.path().join("right.txt")).unwrap(),
        b"head\r\nnew"
    );
}

#[test]
fn equal_headless_inputs_exit_successfully_with_no_hunks() {
    let root = files(&[("left.txt", b"same\n"), ("right.txt", b"same\n")]);
    let output = invoke(
        root.path(),
        &["diff", "left.txt", "right.txt", "--format", "json"],
    );

    assert_eq!(output.status.code(), Some(0));
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["equal"], true);
    assert_eq!(result["hunks"], serde_json::json!([]));
}

#[test]
fn missing_input_reports_failure_instead_of_treating_it_as_an_empty_file() {
    let root = files(&[("right.txt", b"keep\n")]);
    let output = invoke(
        root.path(),
        &["diff", "missing.txt", "right.txt", "--format", "json"],
    );

    assert_eq!(output.status.code(), Some(2));
    assert!(!root.path().join("missing.txt").exists());
    assert_eq!(fs::read(root.path().join("right.txt")).unwrap(), b"keep\n");
}

#[test]
fn headless_merge_writes_independent_changes_to_the_explicit_output_only() {
    let root = files(&[
        ("base.txt", b"one\r\ntwo\r\nthree"),
        ("ours.txt", b"ONE\r\ntwo\r\nthree"),
        ("theirs.txt", b"one\r\ntwo\r\nTHREE"),
    ]);
    let output = invoke(
        root.path(),
        &[
            "merge",
            "--base",
            "base.txt",
            "--ours",
            "ours.txt",
            "--theirs",
            "theirs.txt",
            "--output",
            "result.txt",
            "--non-interactive",
        ],
    );

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        fs::read(root.path().join("result.txt")).unwrap(),
        b"ONE\r\ntwo\r\nTHREE"
    );
    assert_eq!(
        fs::read(root.path().join("base.txt")).unwrap(),
        b"one\r\ntwo\r\nthree"
    );
    assert_eq!(
        fs::read(root.path().join("ours.txt")).unwrap(),
        b"ONE\r\ntwo\r\nthree"
    );
    assert_eq!(
        fs::read(root.path().join("theirs.txt")).unwrap(),
        b"one\r\ntwo\r\nTHREE"
    );
}

#[test]
fn unresolved_headless_merge_preserves_an_existing_output_file() {
    let root = files(&[
        ("base.txt", b"base\n"),
        ("ours.txt", b"ours\n"),
        ("theirs.txt", b"theirs\n"),
        ("result.txt", b"previous reviewed result\n"),
    ]);
    let output = invoke(
        root.path(),
        &[
            "merge",
            "--base",
            "base.txt",
            "--ours",
            "ours.txt",
            "--theirs",
            "theirs.txt",
            "--output",
            "result.txt",
            "--non-interactive",
        ],
    );

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        fs::read(root.path().join("result.txt")).unwrap(),
        b"previous reviewed result\n"
    );
}

#[test]
fn explicit_herdr_gate_outside_herdr_fails_without_accepting_changes() {
    let root = files(&[("note.txt", b"unreviewed\n")]);
    let output = invoke(
        root.path(),
        &[
            "review",
            "--herdr",
            "gate",
            "--agent",
            "missing-agent",
            "--non-interactive",
        ],
    );

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        fs::read(root.path().join("note.txt")).unwrap(),
        b"unreviewed\n"
    );
}

fn git(root: &Path, args: &[&str]) -> Output {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn repository_review_uses_the_selected_base_and_preserves_index_and_worktree() {
    let root = files(&[("note.txt", b"committed\n")]);
    git(root.path(), &["init", "-q"]);
    git(root.path(), &["add", "note.txt"]);
    git(root.path(), &["commit", "-qm", "initial"]);
    fs::write(root.path().join("note.txt"), b"staged\n").unwrap();
    git(root.path(), &["add", "note.txt"]);
    fs::write(root.path().join("note.txt"), b"worktree\n").unwrap();
    fs::write(root.path().join("new 日本.txt"), b"untracked\n").unwrap();

    let output = invoke(
        root.path(),
        &["review", "--base", "index", "--format", "json"],
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    let files = result["files"].as_array().unwrap();
    assert!(
        files
            .iter()
            .any(|file| file["path"] == "note.txt" && file["equal"] == false)
    );
    assert!(files.iter().any(|file| file["path"] == "new 日本.txt"));
    assert_eq!(git(root.path(), &["show", ":note.txt"]).stdout, b"staged\n");
    assert_eq!(
        fs::read(root.path().join("note.txt")).unwrap(),
        b"worktree\n"
    );
}

#[test]
fn difftool_reads_git_environment_without_changing_either_file() {
    let root = files(&[("old.txt", b"before\n"), ("new.txt", b"after\n")]);
    let output = Command::new(env!("CARGO_BIN_EXE_chvrn"))
        .args(["difftool", "--format", "json"])
        .env_remove("HERDR_ENV")
        .env("LOCAL", root.path().join("old.txt"))
        .env("REMOTE", root.path().join("new.txt"))
        .current_dir(root.path())
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["equal"], false);
    assert_eq!(fs::read(root.path().join("old.txt")).unwrap(), b"before\n");
    assert_eq!(fs::read(root.path().join("new.txt")).unwrap(), b"after\n");
}

#[test]
fn mergetool_resolves_git_environment_into_merged_without_overwriting_sources() {
    let root = files(&[
        ("base.txt", b"first\nlast\n"),
        ("local.txt", b"FIRST\nlast\n"),
        ("remote.txt", b"first\nLAST\n"),
        ("merged.txt", b"unresolved output\n"),
    ]);
    let output = Command::new(env!("CARGO_BIN_EXE_chvrn"))
        .args(["mergetool", "--non-interactive"])
        .env_remove("HERDR_ENV")
        .env("BASE", root.path().join("base.txt"))
        .env("LOCAL", root.path().join("local.txt"))
        .env("REMOTE", root.path().join("remote.txt"))
        .env("MERGED", root.path().join("merged.txt"))
        .current_dir(root.path())
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(root.path().join("merged.txt")).unwrap(),
        b"FIRST\nLAST\n"
    );
    assert_eq!(
        fs::read(root.path().join("local.txt")).unwrap(),
        b"FIRST\nlast\n"
    );
    assert_eq!(
        fs::read(root.path().join("remote.txt")).unwrap(),
        b"first\nLAST\n"
    );
}

#[cfg(unix)]
#[test]
fn merge_refuses_symlink_output_without_touching_its_target() {
    let root = files(&[
        ("base.txt", b"same\n"),
        ("ours.txt", b"same\n"),
        ("theirs.txt", b"same\n"),
        ("protected.txt", b"keep\n"),
    ]);
    std::os::unix::fs::symlink("protected.txt", root.path().join("result.txt")).unwrap();
    let output = invoke(
        root.path(),
        &[
            "merge",
            "--base",
            "base.txt",
            "--ours",
            "ours.txt",
            "--theirs",
            "theirs.txt",
            "--output",
            "result.txt",
            "--non-interactive",
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        fs::read(root.path().join("protected.txt")).unwrap(),
        b"keep\n"
    );
    assert!(
        fs::symlink_metadata(root.path().join("result.txt"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}
