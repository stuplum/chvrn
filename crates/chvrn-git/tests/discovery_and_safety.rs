mod support;

use chvrn_git::{Base, ChangeKind, ContentKind, GitError, Repository};
use std::path::PathBuf;
use support::Fixture;

#[test]
fn discovery_preserves_renamed_odd_paths_and_reports_deleted_and_untracked_files() {
    let fixture = Fixture::new();
    let old_name = "old \t日本\n.txt";
    let new_name = "new \t日本\n.txt";
    fixture.write(old_name, b"renamed contents\n");
    fixture.write("deleted.txt", b"removed contents\n");
    fixture.commit_all("initial");
    fixture.git(&["mv", "--", old_name, new_name]);
    std::fs::remove_file(fixture.root().join("deleted.txt")).expect("delete tracked file");
    fixture.write("untracked name.txt", b"new contents\n");

    let repo = Repository::open(fixture.root()).expect("open repository");
    let changes = repo.changes("HEAD").expect("discover all changes");
    assert!(changes.iter().any(|change| {
        change.path == PathBuf::from(new_name)
            && change.old_path == Some(PathBuf::from(old_name))
            && change.kind == ChangeKind::Renamed
            && change.content == ContentKind::Text
    }));
    assert!(changes.iter().any(|change| {
        change.path == PathBuf::from("deleted.txt") && change.kind == ChangeKind::Deleted
    }));
    assert!(changes.iter().any(|change| {
        change.path == PathBuf::from("untracked name.txt") && change.kind == ChangeKind::Added
    }));
    assert_eq!(changes.len(), 3);
}

#[cfg(all(unix, not(target_vendor = "apple")))]
#[test]
fn discovery_preserves_non_utf8_native_path_bytes() {
    use std::os::unix::ffi::OsStringExt;

    let fixture = Fixture::new();
    fixture.write("anchor.txt", b"anchor\n");
    fixture.commit_all("initial");
    let name = PathBuf::from(std::ffi::OsString::from_vec(b"odd-\xff.txt".to_vec()));
    std::fs::write(fixture.root().join(&name), b"new native path\n").expect("write non-UTF-8 path");

    let repo = Repository::open(fixture.root()).expect("open repository");
    let changes = repo.changes("HEAD").expect("discover native path");
    assert!(
        changes
            .iter()
            .any(|change| change.path == name && change.kind == ChangeKind::Added)
    );
    assert_eq!(changes.len(), 1);
}

#[test]
fn binary_and_invalid_utf8_files_are_reported_without_lossy_text_decoding() {
    let fixture = Fixture::new();
    fixture.write("nul.bin", b"before\0after\n");
    fixture.write("invalid.bin", b"valid\xffbyte\n");
    fixture.commit_all("initial");
    fixture.write("nul.bin", b"changed\0after\n");
    fixture.write("invalid.bin", b"changed\xffbyte\n");

    let repo = Repository::open(fixture.root()).expect("open repository");
    let changes = repo.changes("HEAD").expect("discover binary changes");
    assert!(changes.iter().any(|change| {
        change.path == PathBuf::from("nul.bin") && change.content == ContentKind::Binary
    }));
    assert!(changes.iter().any(|change| {
        change.path == PathBuf::from("invalid.bin") && change.content == ContentKind::Binary
    }));
    assert_eq!(fixture.read("nul.bin"), b"changed\0after\n");
    assert_eq!(fixture.read("invalid.bin"), b"changed\xffbyte\n");
}

#[cfg(unix)]
#[test]
fn inspection_refuses_symlink_targets_and_intermediate_symlink_paths() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    fixture.write("safe.txt", b"safe\n");
    fixture.commit_all("initial");
    let outside = fixture.outside();
    std::fs::write(&outside, b"outside\n").expect("create outside file");
    symlink(&outside, fixture.root().join("link.txt")).expect("create file symlink");
    symlink(
        outside.parent().expect("outside parent"),
        fixture.root().join("linked-dir"),
    )
    .expect("create directory symlink");
    let repo = Repository::open(fixture.root()).expect("open repository");

    assert!(matches!(
        repo.review(Base::Index, &[PathBuf::from("link.txt")]),
        Err(GitError::UnsafePath)
    ));
    assert!(matches!(
        repo.review(Base::Index, &[PathBuf::from("linked-dir/outside.txt")]),
        Err(GitError::UnsafePath)
    ));
    assert_eq!(
        std::fs::read(outside).expect("read outside file"),
        b"outside\n"
    );
}

#[cfg(unix)]
#[test]
fn inspection_refuses_a_tracked_symlink_even_if_git_knows_its_target_bytes() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    fixture.write("anchor.txt", b"anchor\n");
    let outside = fixture.outside();
    std::fs::write(&outside, b"outside\n").expect("create outside file");
    symlink(&outside, fixture.root().join("tracked-link")).expect("create symlink");
    fixture.commit_all("tracked link");
    let repo = Repository::open(fixture.root()).expect("open repository");

    assert!(matches!(
        repo.review(Base::Index, &[PathBuf::from("tracked-link")]),
        Err(GitError::UnsafePath)
    ));
    assert_eq!(
        std::fs::read(outside).expect("read outside file"),
        b"outside\n"
    );
}

#[test]
fn inspection_refuses_parent_traversal_and_absolute_paths() {
    let fixture = Fixture::new();
    fixture.write("safe.txt", b"safe\n");
    fixture.commit_all("initial");
    let outside = fixture.outside();
    std::fs::write(&outside, b"outside\n").expect("create outside file");
    let repo = Repository::open(fixture.root()).expect("open repository");

    assert!(matches!(
        repo.review(Base::Index, &[PathBuf::from("../outside.txt")]),
        Err(GitError::UnsafePath)
    ));
    assert!(matches!(
        repo.review(Base::Index, &[outside.clone()]),
        Err(GitError::UnsafePath)
    ));
    assert!(matches!(
        repo.review(Base::Index, &[PathBuf::from(".git/config")]),
        Err(GitError::UnsafePath)
    ));
    assert_eq!(
        std::fs::read(outside).expect("read outside file"),
        b"outside\n"
    );
    assert_eq!(fixture.read("safe.txt"), b"safe\n");
}
