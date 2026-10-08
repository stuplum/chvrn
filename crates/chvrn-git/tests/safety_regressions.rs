mod support;

use chvrn_git::{Base, GitError, Repository};
use std::path::{Path, PathBuf};
use support::Fixture;

#[test]
fn patch_header_like_payload_is_not_a_path() {
    let fixture = Fixture::new();
    fixture.write("file", b"before\n");
    fixture.commit_all("initial");
    let repo = Repository::open(fixture.root()).unwrap();
    let review = repo.review(Base::Index, &[PathBuf::from("file")]).unwrap();
    let patch = b"diff --git a/file b/file\n--- a/file\n+++ b/file\n@@ -1 +1 @@\n-before\n+++ b/../outside\n";
    let candidates = repo.preview_patch(&review, patch).unwrap();
    assert_eq!(candidates[0].path, Path::new("file"));
    assert_eq!(
        candidates[0].bytes.as_deref(),
        Some(b"++ b/../outside\n".as_slice())
    );
    repo.import_patch(&review, patch).unwrap();
    assert_eq!(fixture.read("file"), b"++ b/../outside\n");
}

#[test]
fn patch_accepts_bare_empty_context_lines_without_mistaking_them_for_headers() {
    let fixture = Fixture::new();
    fixture.write("file", b"before\n\n");
    fixture.commit_all("initial");
    let repo = Repository::open(fixture.root()).unwrap();
    let review = repo.review(Base::Index, &[PathBuf::from("file")]).unwrap();
    repo.import_patch(
        &review,
        b"--- a/file\n+++ b/file\n@@ -1,2 +1,2 @@\n-before\n+after\n\n",
    )
    .unwrap();
    assert_eq!(fixture.read("file"), b"after\n\n");
}

#[cfg(unix)]
#[test]
fn failed_temporary_acquisition_preserves_targets_indexes_and_unrelated_paths() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let fixture = Fixture::new();
    fixture.write("file", b"before\n");
    fixture.commit_all("initial");
    fixture.write("file", b"after\n");
    fixture.write("unrelated", b"owned elsewhere");
    std::fs::create_dir(fixture.root().join("unrelated-directory")).unwrap();
    fixture.write("unrelated-directory/sentinel", b"owned elsewhere");
    let repo = Repository::open(fixture.root()).unwrap();
    let review = repo.review(Base::Index, &[PathBuf::from("file")]).unwrap();
    let index = std::fs::read(repo.index_path()).unwrap();
    let root_permissions = std::fs::metadata(fixture.root()).unwrap().permissions();
    std::fs::set_permissions(fixture.root(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let save = repo.save_worktree(&review, Path::new("file"), b"saved\n");
    std::fs::set_permissions(fixture.root(), root_permissions).unwrap();
    assert!(matches!(save, Err(GitError::IoFailure)));
    let index_parent = repo.index_path().parent().unwrap();
    let index_permissions = std::fs::metadata(index_parent).unwrap().permissions();
    std::fs::set_permissions(index_parent, std::fs::Permissions::from_mode(0o500)).unwrap();
    let capture = repo.review(Base::Index, &[PathBuf::from("file")]);
    std::fs::set_permissions(index_parent, index_permissions).unwrap();
    assert!(matches!(capture, Err(GitError::IoFailure)));
    assert_eq!(fixture.read("file"), b"after\n");
    assert_eq!(std::fs::read(repo.index_path()).unwrap(), index);
    assert_eq!(fixture.read("unrelated"), b"owned elsewhere");
    assert_eq!(
        fixture.read("unrelated-directory/sentinel"),
        b"owned elsewhere"
    );
}

#[cfg(unix)]
#[test]
fn executable_deletion_rejection_candidate_matches_restoration() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    fixture.write("run", b"run\n");
    std::fs::set_permissions(
        fixture.root().join("run"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fixture.commit_all("initial");
    std::fs::remove_file(fixture.root().join("run")).unwrap();
    let repo = Repository::open(fixture.root()).unwrap();
    let review = repo
        .review(Base::Revision("HEAD".into()), &[PathBuf::from("run")])
        .unwrap();
    let hunk = review.hunks(Path::new("run"))[0].id;
    let candidate = repo.rejection_candidate(&review, hunk).unwrap();
    repo.reject(&review, &[hunk]).unwrap();
    let after = repo.review_after_changes(&review, &[candidate]).unwrap();
    assert_eq!(after.file(Path::new("run")).unwrap().mode, Some(0o100755));
    assert_eq!(fixture.read("run"), b"run\n");
}

#[cfg(unix)]
#[test]
fn rejection_and_content_patch_preserve_restrictive_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    fixture.write("file", b"before\n");
    fixture.commit_all("initial");
    fixture.write("file", b"after\n");
    std::fs::set_permissions(
        fixture.root().join("file"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let repo = Repository::open(fixture.root()).unwrap();
    let review = repo
        .review(Base::Revision("HEAD".into()), &[PathBuf::from("file")])
        .unwrap();
    repo.reject_file(&review, Path::new("file")).unwrap();
    assert_eq!(
        std::fs::metadata(fixture.root().join("file"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o600
    );
    let review = repo.review(Base::Index, &[PathBuf::from("file")]).unwrap();
    repo.import_patch(
        &review,
        b"diff --git a/file b/file\n--- a/file\n+++ b/file\n@@ -1 +1 @@\n-before\n+after\n",
    )
    .unwrap();
    assert_eq!(
        std::fs::metadata(fixture.root().join("file"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o600
    );
}

#[cfg(unix)]
#[test]
fn inspection_disables_required_executable_filters_and_fsmonitor() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    fixture.write("file", b"before\n");
    fixture.write(
        ".gitattributes",
        b"file filter=hostile diff=hostile text eol=lf\n",
    );
    fixture.commit_all("initial");
    fixture.write("file", b"after\r\n");
    let marker = fixture.outside();
    let script = fixture.root().join(".git/hostile");
    std::fs::write(
        &script,
        format!("#!/bin/sh\ntouch '{}'\ncat\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    fixture.git(&["config", "core.fsmonitor", script.to_str().unwrap()]);
    fixture.git(&["config", "filter.hostile.clean", script.to_str().unwrap()]);
    fixture.git(&["config", "filter.hostile.required", "true"]);
    fixture.git(&["config", "diff.external", script.to_str().unwrap()]);
    fixture.git(&["config", "diff.hostile.textconv", script.to_str().unwrap()]);
    let repo = Repository::open(fixture.root()).unwrap();
    let changes = repo.changes("HEAD").unwrap();
    assert!(
        changes
            .iter()
            .any(|change| change.path == Path::new("file"))
    );
    repo.review(Base::Index, &[]).unwrap();
    assert!(!marker.exists(), "repository executable config ran");
    fixture.write("file", b"before\r\n");
    assert!(repo.changes("HEAD").unwrap().is_empty());
    std::fs::write(
        &script,
        format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
    )
    .unwrap();
    fixture.git(&["config", "filter.hostile.process", script.to_str().unwrap()]);
    fixture.write("file", b"process\r\n");
    assert!(
        repo.changes("HEAD")
            .unwrap()
            .iter()
            .any(|change| change.path == Path::new("file"))
    );
    assert!(!marker.exists(), "repository process filter ran");
}

#[cfg(unix)]
fn child_scenario(name: &str) {
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "isolated_git_safety_child", "--nocapture"])
        .env("CHVRN_SAFETY_SCENARIO", name)
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "scenario {name} failed");
            return;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("scenario {name} blocked");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(unix)]
#[test]
fn fifo_inspection_is_bounded() {
    child_scenario("fifo");
}

#[cfg(unix)]
#[test]
fn unowned_worktree_and_index_collisions_survive() {
    child_scenario("collision");
}

#[cfg(unix)]
#[test]
fn capture_uses_one_index_even_during_a_to_b_to_a_change() {
    child_scenario("index-race");
}

#[cfg(unix)]
#[test]
fn isolated_git_safety_child() {
    use std::os::unix::fs::PermissionsExt;
    let Ok(scenario) = std::env::var("CHVRN_SAFETY_SCENARIO") else {
        return;
    };
    let fixture = Fixture::new();
    fixture.write("file", b"before\n");
    if scenario == "index-race" {
        std::fs::set_permissions(
            fixture.root().join("file"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    fixture.commit_all("initial");
    fixture.write("file", b"after\n");
    let repo = Repository::open(fixture.root()).unwrap();
    if scenario == "fifo" {
        let review = repo.review(Base::Index, &[PathBuf::from("file")]).unwrap();
        std::fs::remove_file(fixture.root().join("file")).unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(fixture.root().join("file"))
                .status()
                .unwrap()
                .success()
        );
        assert!(matches!(
            repo.validate_review(&review),
            Err(GitError::UnsafePath)
        ));
    } else if scenario == "collision" {
        let review = repo.review(Base::Index, &[PathBuf::from("file")]).unwrap();
        let mut sentinels = Vec::new();
        for n in 1..16 {
            for path in [
                fixture
                    .root()
                    .join(format!(".chvrn-{}-{n}", std::process::id())),
                repo.index_path()
                    .with_file_name(format!("chvrn-index-{}-{n}", std::process::id())),
            ] {
                if n % 2 == 0 {
                    std::fs::create_dir(&path).unwrap();
                    std::fs::write(path.join("sentinel"), b"owned elsewhere").unwrap();
                    sentinels.push(path.join("sentinel"));
                } else {
                    std::fs::write(&path, b"owned elsewhere").unwrap();
                    sentinels.push(path);
                }
            }
        }
        let index = std::fs::read(repo.index_path()).unwrap();
        let save = repo.save_worktree(&review, Path::new("file"), b"saved\n");
        for path in &sentinels {
            assert_eq!(std::fs::read(path).unwrap(), b"owned elsewhere");
        }
        assert_eq!(std::fs::read(repo.index_path()).unwrap(), index);
        if save.is_err() {
            assert_eq!(fixture.read("file"), b"after\n");
        }
        let review = repo.review(Base::Index, &[PathBuf::from("file")]).unwrap();
        let stage = repo.stage_file(&review, Path::new("file"));
        for path in &sentinels {
            assert_eq!(std::fs::read(path).unwrap(), b"owned elsewhere");
        }
        if stage.is_err() {
            assert_eq!(std::fs::read(repo.index_path()).unwrap(), index);
        }
    } else if scenario == "index-race" {
        let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|path| path.join("git"))
            .find(|path| path.is_file())
            .unwrap();
        let bin = tempfile::tempdir().unwrap();
        let wrapper = bin.path().join("git");
        std::fs::write(&wrapper, format!("#!/bin/sh\ncase \" $* \" in\n*' ls-files '* )\n  cp .git/index .git/saved-index\n  env -u GIT_INDEX_FILE '{}' update-index --chmod=-x file\n  '{}' \"$@\"\n  result=$?\n  mv .git/saved-index .git/index\n  exit $result\n;;\nesac\nexec '{}' \"$@\"\n", real_git.display(), real_git.display(), real_git.display())).unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::join_paths(
            std::iter::once(bin.path().to_path_buf())
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        unsafe {
            std::env::set_var("PATH", path);
        }
        let review = repo.review(Base::Index, &[PathBuf::from("file")]).unwrap();
        assert_eq!(
            review.file(Path::new("file")).unwrap().base_mode,
            Some(0o100755)
        );
        repo.stage(&review, &[review.hunks(Path::new("file"))[0].id])
            .unwrap();
        let output = std::process::Command::new(real_git)
            .current_dir(fixture.root())
            .args(["ls-files", "--stage", "file"])
            .output()
            .unwrap();
        assert!(output.stdout.starts_with(b"100755 "));
    }
}

#[test]
fn absent_split_and_linked_worktree_indexes_stage_reviewed_bytes() {
    let fixture = Fixture::new();
    fixture.write("new", b"new\n");
    let repo = Repository::open(fixture.root()).unwrap();
    assert!(!repo.index_path().exists());
    let review = repo.review(Base::Index, &[]).unwrap();
    repo.stage_file(&review, Path::new("new")).unwrap();
    assert_eq!(fixture.git(&["show", ":new"]), b"new\n");
    fixture.commit_all("initial");
    fixture.git(&["update-index", "--split-index"]);
    fixture.git(&["config", "core.splitIndex", "true"]);
    fixture.write("new", b"split\n");
    let review = repo.review(Base::Index, &[]).unwrap();
    repo.stage_file(&review, Path::new("new")).unwrap();
    assert_eq!(fixture.git(&["show", ":new"]), b"split\n");
    let linked = fixture.outside();
    fixture.git(&[
        "worktree",
        "add",
        "--detach",
        linked.to_str().unwrap(),
        "HEAD",
    ]);
    let repo = Repository::open(&linked).unwrap();
    std::fs::write(linked.join("new"), b"linked\n").unwrap();
    let review = repo.review(Base::Index, &[]).unwrap();
    repo.stage_file(&review, Path::new("new")).unwrap();
    let output = std::process::Command::new("git")
        .current_dir(&linked)
        .args(["show", ":new"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"linked\n");
    assert_eq!(fixture.git(&["show", ":new"]), b"split\n");
}

#[test]
fn private_index_preserves_racy_clean_detection() {
    let fixture = Fixture::new();
    fixture.write("file", b"before\n");
    fixture.commit_all("initial");
    fixture.git(&["config", "core.trustctime", "false"]);
    let path = fixture.root().join("file");
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    let times = std::fs::FileTimes::new().set_modified(modified);
    fixture.write("file", b"after!\n");
    std::fs::File::open(&path)
        .unwrap()
        .set_times(times)
        .unwrap();
    let repo = Repository::open(fixture.root()).unwrap();
    std::fs::File::open(repo.index_path())
        .unwrap()
        .set_times(times)
        .unwrap();
    let changes = repo.changes("HEAD").unwrap();
    assert!(
        changes
            .iter()
            .any(|change| change.path == Path::new("file"))
    );
    let review = repo.review(Base::Index, &[]).unwrap();
    assert_eq!(
        review.file(Path::new("file")).unwrap().worktree.as_deref(),
        Some(b"after!\n".as_slice())
    );
}
