use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;
use std::time::Duration;

#[tokio::test]
async fn headless_companion_completes_and_reaps_all_herdr_commands() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let git = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(root)
        .status()
        .unwrap();
    assert!(git.success());
    let binary = root.join("herdr-fixture");
    let pids = root.join("pids");
    let command = root.join("companion-command");
    fs::write(&binary, format!(
        "#!/bin/sh\nprintf '%s\\n' $$ >> '{pids}'\ncase \"$1 $2\" in\n'agent get') printf '%s' '{{\"result\":{{\"agent\":{{\"pane_id\":\"agent-pane\"}}}}}}';;\n'pane split') printf '%s' '{{\"result\":{{\"pane\":{{\"pane_id\":\"review-pane\"}}}}}}';;\n'pane run') [ \"$3\" = review-pane ] || exit 2; printf '%s' \"$4\" > '{command}';;\n*) exit 2;;\nesac\n",
        pids = pids.display(), command = command.display(),
    )).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_chvrn"))
        .args([
            "review",
            "--base",
            "index",
            "--open-companion",
            "--agent",
            "selected-agent",
        ])
        .current_dir(root)
        .env("HERDR_ENV", "1")
        .env("HERDR_PANE_ID", "caller-pane")
        .env("HERDR_BIN_PATH", &binary)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let output = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("Opened chvrn companion in review-pane")
    );
    let requested = fs::read_to_string(command).unwrap();
    assert!(requested.contains("'--agent' 'agent-pane'"));
    assert!(requested.contains("'--base' 'index'"));
    let pids: Vec<libc::pid_t> = fs::read_to_string(pids)
        .unwrap()
        .lines()
        .map(|pid| pid.parse().unwrap())
        .collect();
    assert_eq!(pids.len(), 3);
    for pid in pids {
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
}
