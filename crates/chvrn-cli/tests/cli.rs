use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

fn command(root: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_chvrn"));
    command
        .args(args)
        .current_dir(root)
        .env_remove("CHVRN_BASE_BRANCH")
        .env_remove("HERDR_ENV")
        .env_remove("HERDR_PANE_ID")
        .env_remove("HERDR_WORKSPACE_ID")
        .env_remove("HERDR_TAB_ID");
    command
}

fn invoke(root: &Path, args: &[&str]) -> Output {
    command(root, args).output().expect("run chvrn")
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

fn repository(entries: &[(&str, &[u8])]) -> TempDir {
    let root = files(entries);
    git(root.path(), &["init", "-q"]);
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "initial"]);
    root
}

#[test]
fn default_review_includes_committed_and_uncommitted_work_but_not_main_only_changes() {
    let root = repository(&[("note.txt", b"initial\n")]);
    git(root.path(), &["branch", "-M", "main"]);
    let base = git(root.path(), &["rev-parse", "HEAD"]);
    git(root.path(), &["checkout", "-qb", "task"]);
    fs::write(root.path().join("committed.txt"), b"task change\n").unwrap();
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "task"]);
    git(root.path(), &["checkout", "-q", "main"]);
    fs::write(root.path().join("main-only.txt"), b"upstream change\n").unwrap();
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "upstream"]);
    git(root.path(), &["checkout", "-q", "task"]);
    fs::write(root.path().join("note.txt"), b"staged\n").unwrap();
    git(root.path(), &["add", "note.txt"]);
    fs::write(root.path().join("note.txt"), b"unstaged\n").unwrap();
    fs::write(root.path().join("untracked.txt"), b"new\n").unwrap();

    for args in [
        &["review", "--format", "json"][..],
        &["--format", "json"][..],
    ] {
        let output = invoke(root.path(), args);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["base"], String::from_utf8_lossy(&base.stdout).trim());
        let paths: Vec<_> = result["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|file| file["path"].as_str().unwrap())
            .collect();
        assert_eq!(paths, ["committed.txt", "note.txt", "untracked.txt"]);
    }
    assert_eq!(git(root.path(), &["show", ":note.txt"]).stdout, b"staged\n");
    assert_eq!(
        fs::read(root.path().join("note.txt")).unwrap(),
        b"unstaged\n"
    );
}

#[test]
fn branch_override_reviews_only_changes_since_the_selected_branch_fork() {
    let root = repository(&[("note.txt", b"initial\n")]);
    git(root.path(), &["branch", "-M", "main"]);
    git(root.path(), &["checkout", "-qb", "develop"]);
    fs::write(root.path().join("develop.txt"), b"development\n").unwrap();
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "development"]);
    let base = git(root.path(), &["rev-parse", "HEAD"]);
    git(root.path(), &["checkout", "-qb", "task"]);
    fs::write(root.path().join("task.txt"), b"task\n").unwrap();
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "task"]);
    git(root.path(), &["checkout", "-q", "develop"]);
    fs::write(root.path().join("upstream.txt"), b"upstream\n").unwrap();
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "upstream"]);
    git(root.path(), &["checkout", "-q", "task"]);

    let output = command(root.path(), &["review", "--format", "json"])
        .env("CHVRN_BASE_BRANCH", "develop")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["base"], String::from_utf8_lossy(&base.stdout).trim());
    let paths: Vec<_> = result["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["task.txt"]);
}

#[test]
fn explicit_base_bypasses_branch_override_and_preserves_direct_revision_comparison() {
    let root = repository(&[("note.txt", b"initial\n")]);
    git(root.path(), &["branch", "-M", "main"]);
    git(root.path(), &["checkout", "-qb", "task"]);
    git(root.path(), &["checkout", "-q", "main"]);
    fs::write(root.path().join("main-only.txt"), b"upstream\n").unwrap();
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "upstream"]);
    git(root.path(), &["checkout", "-q", "task"]);

    for (base, code, paths) in [
        ("index", 0, vec![]),
        ("HEAD", 0, vec![]),
        ("main", 1, vec!["main-only.txt"]),
    ] {
        let output = command(root.path(), &["review", "--base", base, "--format", "json"])
            .env("CHVRN_BASE_BRANCH", "missing")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(code), "{output:?}");
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["base"], base);
        let actual: Vec<_> = result["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|file| file["path"].as_str().unwrap())
            .collect();
        assert_eq!(actual, paths);
    }
}

#[test]
fn missing_main_fails_instead_of_reviewing_the_index() {
    let root = repository(&[("note.txt", b"initial\n")]);
    git(root.path(), &["branch", "-M", "task"]);
    let output = invoke(root.path(), &["review"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("main"));
}

#[test]
fn invalid_branch_override_fails_without_falling_back_to_main() {
    let root = repository(&[("note.txt", b"initial\n")]);
    git(root.path(), &["branch", "-M", "main"]);
    for branch in ["missing", "", "--all"] {
        let output = command(root.path(), &["review"])
            .env("CHVRN_BASE_BRANCH", branch)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("CHVRN_BASE_BRANCH"));
    }
}

#[test]
fn unrelated_branch_fails_instead_of_reviewing_a_different_base() {
    let root = repository(&[("note.txt", b"initial\n")]);
    git(root.path(), &["branch", "-M", "main"]);
    git(root.path(), &["checkout", "--orphan", "unrelated"]);
    git(root.path(), &["commit", "-qm", "independent root"]);
    let output = invoke(root.path(), &["review"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("merge base"));
}

fn assert_metadata_difference(output: Output, path: &str) {
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["files"][0]["path"], path);
    assert_eq!(result["files"][0]["equal"], false);
    assert_eq!(result["files"][0]["hunks"], serde_json::json!([]));
}

#[test]
fn repository_review_detects_empty_file_additions() {
    let root = repository(&[("note.txt", b"same\n")]);
    fs::write(root.path().join("empty.txt"), b"").unwrap();

    for base in ["index", "HEAD"] {
        assert_metadata_difference(
            invoke(root.path(), &["review", "--base", base]),
            "empty.txt",
        );
        let output = invoke(root.path(), &["review", "--base", base, "--format", "text"]);
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stdout).starts_with("! "));
    }
    assert_eq!(fs::read(root.path().join("empty.txt")).unwrap(), b"");
    assert_eq!(git(root.path(), &["ls-files"]).stdout, b"note.txt\n");
}

#[test]
fn repository_review_marks_deleted_empty_files_as_different() {
    let root = repository(&[("empty.txt", b"")]);
    fs::remove_file(root.path().join("empty.txt")).unwrap();

    for base in ["index", "HEAD"] {
        assert_metadata_difference(
            invoke(root.path(), &["review", "--base", base]),
            "empty.txt",
        );
    }
}

#[cfg(unix)]
#[test]
fn repository_review_detects_permission_changes_without_text_changes() {
    use std::os::unix::fs::PermissionsExt;

    let root = files(&[("script.sh", b"echo hello\n")]);
    let path = root.path().join("script.sh");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    git(root.path(), &["init", "-q"]);
    git(root.path(), &["config", "core.filemode", "true"]);
    git(root.path(), &["add", "script.sh"]);
    git(root.path(), &["commit", "-qm", "initial"]);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();

    for base in ["index", "HEAD"] {
        assert_metadata_difference(
            invoke(root.path(), &["review", "--base", base]),
            "script.sh",
        );
    }
    assert_eq!(fs::read(&path).unwrap(), b"echo hello\n");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert!(
        git(root.path(), &["ls-files", "--stage"])
            .stdout
            .starts_with(b"100644 ")
    );
}

#[cfg(unix)]
#[test]
fn whitespace_filtering_does_not_hide_permission_changes() {
    use std::os::unix::fs::PermissionsExt;

    let root = files(&[("note.txt", b"same\n")]);
    let path = root.path().join("note.txt");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    git(root.path(), &["init", "-q"]);
    git(root.path(), &["config", "core.filemode", "true"]);
    git(root.path(), &["add", "note.txt"]);
    git(root.path(), &["commit", "-qm", "initial"]);
    fs::write(&path, b"  same  \n").unwrap();

    let args = [
        "review",
        "--base",
        "index",
        "--whitespace",
        "ignore-edge",
        "note.txt",
    ];
    let output = invoke(root.path(), &args);
    assert_eq!(output.status.code(), Some(0));
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["files"][0]["equal"], true);

    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    assert_metadata_difference(invoke(root.path(), &args), "note.txt");
}

#[test]
fn patch_preview_detects_empty_file_creation_and_deletion_without_applying_them() {
    for (mode, existing) in [("new", false), ("deleted", true)] {
        let root = repository(&[("note.txt", b"same\n")]);
        let patch = tempfile::NamedTempFile::new().unwrap();
        fs::write(
            patch.path(),
            format!("diff --git a/empty.txt b/empty.txt\n{mode} file mode 100644\n"),
        )
        .unwrap();
        if existing {
            fs::write(root.path().join("empty.txt"), b"").unwrap();
        }

        assert_metadata_difference(
            invoke(
                root.path(),
                &[
                    "review",
                    "--base",
                    "index",
                    "--patch",
                    patch.path().to_str().unwrap(),
                ],
            ),
            "empty.txt",
        );
        assert_eq!(root.path().join("empty.txt").exists(), existing);
    }
}

#[cfg(unix)]
#[test]
fn patch_preview_detects_permission_changes_without_applying_them() {
    use std::os::unix::fs::PermissionsExt;

    let root = repository(&[("script.sh", b"echo hello\n")]);
    let path = root.path().join("script.sh");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    let patch = tempfile::NamedTempFile::new().unwrap();
    fs::write(
        patch.path(),
        b"diff --git a/script.sh b/script.sh\nold mode 100644\nnew mode 100755\n",
    )
    .unwrap();

    assert_metadata_difference(
        invoke(
            root.path(),
            &[
                "review",
                "--base",
                "index",
                "--patch",
                patch.path().to_str().unwrap(),
            ],
        ),
        "script.sh",
    );
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o644
    );
}

#[test]
fn headless_review_rejects_report_and_export_requests_without_touching_destinations() {
    let root = repository(&[("note.txt", b"same\n")]);
    fs::write(root.path().join("note.txt"), b"changed\n").unwrap();
    let destination = tempfile::NamedTempFile::new().unwrap();
    fs::write(destination.path(), b"previous output\n").unwrap();

    for flag in ["--report", "--export-patch"] {
        let output = invoke(
            root.path(),
            &[
                "review",
                "--base",
                "HEAD",
                flag,
                destination.path().to_str().unwrap(),
            ],
        );
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(flag));
        assert!(error.contains("interactive"));
        assert_eq!(fs::read(destination.path()).unwrap(), b"previous output\n");
        assert_eq!(
            fs::read(root.path().join("note.txt")).unwrap(),
            b"changed\n"
        );
        assert_eq!(git(root.path(), &["show", ":note.txt"]).stdout, b"same\n");
    }
}
