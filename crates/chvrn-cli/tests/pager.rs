use std::io::Write;
use std::process::{Command, Output, Stdio};

fn pager(input: &[u8], args: &[&str]) -> Output {
    let root = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_chvrn"))
        .arg("pager")
        .args(args)
        .current_dir(root.path())
        .env("HOME", root.path())
        .env("XDG_CONFIG_HOME", root.path())
        .env("HERDR_ENV", "1")
        .env("HERDR_PANE_ID", "not-an-agent")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn redirected_pager_preserves_patch_bytes_outside_a_repository() {
    let patch = b"\x1b[1mdiff --git a/note.txt b/note.txt\x1b[0m\n--- a/note.txt\n+++ b/note.txt\n@@ -5 +5 @@\n-before\r\n+after\r\n\\ No newline at end of file\n";
    let output = pager(patch, &[]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(output.stdout, patch);
    assert!(output.stderr.is_empty());
}

#[test]
fn redirected_pager_preserves_non_utf8_input_without_attempting_to_parse_it() {
    let input = b"binary \0\xff payload\n";
    let output = pager(input, &["--non-interactive"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(output.stdout, input);
}

#[test]
fn empty_pager_input_exits_successfully_without_output() {
    let output = pager(b"", &[]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn pager_rejects_repository_integrations_instead_of_starting_them() {
    for args in [
        &["--jev"][..],
        &["--agent", "another-pane"][..],
        &["--herdr", "auto"][..],
        &["--lsp", "missing-language-server"][..],
    ] {
        let output = pager(b"", args);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty());
        let message = String::from_utf8(output.stderr).unwrap();
        assert!(message.contains("pager"), "{message}");
        assert!(message.contains(args[0]), "{message}");
    }
}

#[test]
fn pager_rejects_json_output_instead_of_emitting_a_patch_as_json() {
    let output = pager(b"", &["--format", "json"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty());
    let message = String::from_utf8(output.stderr).unwrap();
    assert!(message.contains("json"), "{message}");
}
