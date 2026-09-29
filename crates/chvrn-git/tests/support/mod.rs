use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tempfile::TempDir;

pub struct Fixture {
    home: TempDir,
    root: PathBuf,
}

impl Fixture {
    pub fn new() -> Self {
        let home = tempfile::tempdir().expect("create isolated fixture");
        let root = home.path().join("repository");
        fs::create_dir(&root).expect("create repository directory");
        let fixture = Self { home, root };
        fixture.git(&["init", "-q"]);
        fixture.git(&["config", "--local", "user.name", "Fixture Author"]);
        fixture.git(&["config", "--local", "user.email", "fixture@example.invalid"]);
        fixture.git(&["config", "--local", "core.filemode", "true"]);
        fixture.git(&["config", "--local", "core.hooksPath", "/dev/null"]);
        fixture
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn outside(&self) -> PathBuf {
        self.home.path().join("outside.txt")
    }

    pub fn write(&self, path: &str, bytes: &[u8]) {
        let target = self.root.join(path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).expect("create file parent");
        }
        fs::write(target, bytes).expect("write fixture file");
    }

    pub fn read(&self, path: &str) -> Vec<u8> {
        fs::read(self.root.join(path)).expect("read fixture file")
    }

    pub fn git(&self, args: &[&str]) -> Vec<u8> {
        self.git_os(&args.iter().map(OsStr::new).collect::<Vec<_>>())
    }

    pub fn git_with_input(&self, args: &[&str], input: &[u8]) -> Vec<u8> {
        let mut child = self
            .git_command()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run Git with input");
        child
            .stdin
            .take()
            .expect("Git standard input")
            .write_all(input)
            .expect("send Git input");
        let output = child.wait_with_output().expect("wait for Git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    fn git_os(&self, args: &[&OsStr]) -> Vec<u8> {
        let output = self.git_command().args(args).output().expect("run Git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    fn git_command(&self) -> Command {
        let mut command = Command::new("git");
        command
            .current_dir(&self.root)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").expect("PATH for Git"))
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                self.home.path().join("empty-gitconfig"),
            )
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "core.hooksPath")
            .env("GIT_CONFIG_VALUE_0", self.home.path().join("no-hooks"))
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_NO_REPLACE_OBJECTS", "1");
        command
    }

    pub fn commit_all(&self, message: &str) {
        self.git(&["add", "--all"]);
        self.git(&["commit", "-q", "-m", message]);
    }

    pub fn head(&self) -> String {
        String::from_utf8(self.git(&["rev-parse", "HEAD"]))
            .expect("ASCII object ID")
            .trim()
            .to_owned()
    }
}
