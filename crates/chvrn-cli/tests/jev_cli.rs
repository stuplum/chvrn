use std::fs;
use std::process::{Command, Output};
use tempfile::TempDir;

fn fixture() -> TempDir {
    let root = tempfile::tempdir().unwrap();
    for (name, text) in [
        ("base.txt", "one\r\ntwo\r\nthree"),
        ("ours.txt", "ONE\r\ntwo\r\nthree"),
        ("theirs.txt", "one\r\ntwo\r\nTHREE"),
    ] {
        fs::write(root.path().join(name), text).unwrap();
    }
    root
}

fn command(root: &TempDir) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_chvrn"));
    command
        .current_dir(root.path())
        .env("XDG_CONFIG_HOME", root.path().join("config-home"))
        .env_remove("TYPESAFE_API_KEY")
        .env_remove("HERDR_ENV")
        .env_remove("HERDR_PANE_ID")
        .env_remove("HERDR_WORKSPACE_ID")
        .env_remove("HERDR_TAB_ID")
        .env_remove("BASE")
        .env_remove("LOCAL")
        .env_remove("REMOTE")
        .env_remove("MERGED");
    command
}

fn merge(root: &TempDir) -> Command {
    let mut command = command(root);
    command.args([
        "merge",
        "--base",
        "base.txt",
        "--ours",
        "ours.txt",
        "--theirs",
        "theirs.txt",
        "--output",
        "result.txt",
    ]);
    command
}

fn assert_interactive_refusal(output: &Output) {
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--jev"), "{stderr}");
    assert!(stderr.contains("interactive"), "{stderr}");
    assert!(!stderr.contains("fixture-secret"), "{stderr}");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("fixture-secret"));
}

fn assert_original_inputs(root: &TempDir) {
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
fn an_unusable_key_does_not_enable_or_block_ordinary_merging() {
    let root = fixture();
    let output = merge(&root)
        .env("TYPESAFE_API_KEY", "fixture-secret\r\nnot-a-valid-header")
        .arg("--non-interactive")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        fs::read(root.path().join("result.txt")).unwrap(),
        b"ONE\r\ntwo\r\nTHREE"
    );
    assert_original_inputs(&root);
    assert!(!String::from_utf8_lossy(&output.stderr).contains("fixture-secret"));
}

#[test]
fn explicit_headless_assistance_refuses_to_overwrite_even_a_conflict_free_result() {
    let root = fixture();
    fs::write(root.path().join("result.txt"), b"already reviewed\r\n").unwrap();
    let output = merge(&root)
        .env("TYPESAFE_API_KEY", "fixture-secret")
        .args(["--jev", "--non-interactive"])
        .output()
        .unwrap();

    assert_interactive_refusal(&output);
    assert_eq!(
        fs::read(root.path().join("result.txt")).unwrap(),
        b"already reviewed\r\n"
    );
    assert_original_inputs(&root);
}

#[test]
fn redirected_assistance_refuses_before_creating_an_output_file() {
    let root = fixture();
    let output = merge(&root)
        .env("TYPESAFE_API_KEY", "fixture-secret")
        .arg("--jev")
        .output()
        .unwrap();

    assert_interactive_refusal(&output);
    assert!(!root.path().join("result.txt").exists());
    assert_original_inputs(&root);
}

#[test]
fn headless_mergetool_assistance_preserves_the_git_selected_output() {
    let root = fixture();
    fs::write(root.path().join("git-merged.txt"), b"reviewed Git result\n").unwrap();
    let output = command(&root)
        .env("TYPESAFE_API_KEY", "fixture-secret")
        .env("BASE", root.path().join("base.txt"))
        .env("LOCAL", root.path().join("ours.txt"))
        .env("REMOTE", root.path().join("theirs.txt"))
        .env("MERGED", root.path().join("git-merged.txt"))
        .args(["mergetool", "--jev"])
        .output()
        .unwrap();

    assert_interactive_refusal(&output);
    assert_eq!(
        fs::read(root.path().join("git-merged.txt")).unwrap(),
        b"reviewed Git result\n"
    );
    assert_original_inputs(&root);
}

#[test]
fn headless_diff_assistance_refuses_without_touching_either_input() {
    let root = fixture();
    let output = command(&root)
        .env("TYPESAFE_API_KEY", "fixture-secret")
        .args(["diff", "ours.txt", "theirs.txt", "--jev"])
        .output()
        .unwrap();

    assert_interactive_refusal(&output);
    assert_original_inputs(&root);
    assert!(!root.path().join("result.txt").exists());
}

#[test]
fn global_assistance_before_merge_refuses_before_output_creation() {
    let root = fixture();
    let output = command(&root)
        .env("TYPESAFE_API_KEY", "fixture-secret")
        .args([
            "--jev",
            "merge",
            "--base",
            "base.txt",
            "--ours",
            "ours.txt",
            "--theirs",
            "theirs.txt",
            "--output",
            "result.txt",
            "--format",
            "json",
        ])
        .output()
        .unwrap();

    assert_interactive_refusal(&output);
    assert!(!root.path().join("result.txt").exists());
    assert_original_inputs(&root);
}

#[test]
fn headless_repository_assistance_refuses_before_discovery_or_side_effects() {
    for args in [
        vec!["--jev"],
        vec!["--jev", "--non-interactive"],
        vec!["--jev", "review", "--non-interactive"],
        vec!["review", "--jev", "--format", "text"],
        vec!["review", "--format", "json", "--jev"],
        vec![
            "review",
            "--report",
            "report.json",
            "--export-patch",
            "review.patch",
            "--open-companion",
            "--jev",
        ],
    ] {
        let root = fixture();
        fs::write(root.path().join("report.json"), b"existing report\n").unwrap();
        fs::write(root.path().join("review.patch"), b"existing patch\n").unwrap();
        let output = command(&root)
            .env("TYPESAFE_API_KEY", "fixture-secret")
            .args(args)
            .output()
            .unwrap();

        assert_interactive_refusal(&output);
        assert!(output.stdout.is_empty());
        assert_eq!(
            fs::read(root.path().join("report.json")).unwrap(),
            b"existing report\n"
        );
        assert_eq!(
            fs::read(root.path().join("review.patch")).unwrap(),
            b"existing patch\n"
        );
        assert_original_inputs(&root);
    }
}
