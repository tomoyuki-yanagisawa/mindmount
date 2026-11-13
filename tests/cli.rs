use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use mindmount::config::{Config, Credentials};
use tempfile::TempDir;

struct TestEnv {
    temp: TempDir,
    config_home: PathBuf,
    project: PathBuf,
    notes: PathBuf,
    daemon: RefCell<Option<Child>>,
}

impl TestEnv {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let config_home = temp.path().join("config");
        let project = temp.path().join("project");
        let notes = project.join("notes");
        fs::create_dir_all(notes.join("decisions")).unwrap();
        fs::write(
            notes.join("index.md"),
            "# Index\n\nSee [Architecture](./architecture.md).\n",
        )
        .unwrap();
        fs::write(
            notes.join("architecture.md"),
            "---\ntitle: Architecture\n---\n\n# Architecture\n",
        )
        .unwrap();
        fs::write(notes.join("decisions/storage.md"), "# Storage\n").unwrap();

        Self {
            temp,
            config_home,
            project,
            notes,
            daemon: RefCell::new(None),
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mindmount"));
        command.env("XDG_CONFIG_HOME", &self.config_home);
        command.env("XDG_DATA_HOME", self.temp.path().join("data"));
        command.env("XDG_STATE_HOME", self.temp.path().join("state"));
        command.env("XDG_CACHE_HOME", self.temp.path().join("cache"));
        command.env("HOME", self.temp.path());
        command.env("MINDMOUNT_DISABLE_SERVICE", "1");
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        if needs_daemon(args) {
            self.ensure_daemon();
        }
        self.command()
            .args(args)
            .output()
            .expect("failed to run mindmount")
    }

    fn run_native(&self, args: &[&str]) -> Output {
        self.command()
            .args(args)
            .output()
            .expect("failed to run mindmount")
    }

    fn ensure_daemon(&self) {
        if self.daemon.borrow().is_some() {
            return;
        }
        let child = self
            .command()
            .args(["daemon", "run", "--port", "0"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to start the test daemon");
        self.daemon.replace(Some(child));
        let deadline = Instant::now() + Duration::from_secs(15);
        while !self.endpoint_path().is_file() {
            assert!(Instant::now() < deadline, "test daemon did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn stop_daemon(&self) -> Option<Output> {
        let mut child = self.daemon.borrow_mut().take()?;
        let stop = self
            .command()
            .args(["daemon", "stop"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to stop the test daemon");
        // Reap the daemon while `daemon stop` waits for it, so it does not
        // remain a zombie that `kill -0` still reports as running.
        let _ = child.wait();
        stop.wait_with_output().ok()
    }

    fn endpoint_path(&self) -> PathBuf {
        self.temp.path().join("state/mindmount/daemon.endpoint")
    }

    fn endpoint(&self) -> String {
        fs::read_to_string(self.endpoint_path())
            .expect("daemon endpoint")
            .trim()
            .to_string()
    }

    fn port(&self) -> u16 {
        self.endpoint().rsplit(':').next().unwrap().parse().unwrap()
    }

    fn config_path(&self) -> PathBuf {
        self.config_home.join("mindmount/config.toml")
    }

    fn database_path(&self) -> PathBuf {
        self.temp.path().join("data/mindmount/mindmount.sqlite")
    }

    fn load_config(&self) -> Config {
        Config::load(&self.config_path()).unwrap()
    }

    fn mount(&self, name: &str) -> Output {
        self.run(&[
            "mount",
            "add",
            "-p",
            self.project.to_str().unwrap(),
            "-d",
            "./notes",
            "-n",
            name,
        ])
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        let _ = self.stop_daemon();
    }
}

fn needs_daemon(args: &[&str]) -> bool {
    !matches!(args.first(), None | Some(&"daemon") | Some(&"version"))
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_failure(output: &Output) {
    assert!(!output.status.success(), "command unexpectedly succeeded");
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(directory: &Path, base: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let mut entries: Vec<_> = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect();
        entries.sort_by_key(|entry| entry.file_name());

        for entry in entries {
            let path = entry.path();
            let relative = path.strip_prefix(base).unwrap().to_path_buf();
            if path.is_dir() {
                walk(&path, base, files);
            } else {
                files.insert(relative, fs::read(&path).unwrap());
            }
        }
    }

    let mut files = BTreeMap::new();
    walk(root, root, &mut files);
    files
}

fn wait_for_pages(env: &TestEnv, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let output = env.run(&["index", "pages"]);
        if output.status.success() && stdout(&output).contains(expected) {
            return;
        }
        assert!(Instant::now() < deadline, "watch did not index {expected}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for_pages_absent(env: &TestEnv, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let output = env.run(&["index", "pages"]);
        if output.status.success() && !stdout(&output).contains(expected) {
            return;
        }
        assert!(Instant::now() < deadline, "watch did not remove {expected}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn git(project: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(project)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .unwrap();
    assert_success(&output);
}

#[test]
fn watch_eventually_reindexes_after_temporary_file_absence() {
    let env = TestEnv::new();
    fs::write(
        env.notes.join("index.md"),
        "---\nid: 424242\n---\n# Index\n",
    )
    .unwrap();
    assert_success(&env.mount("Notes"));
    fs::write(env.notes.join("ready.md"), "# Watch ready\n").unwrap();
    wait_for_pages(&env, "Watch ready");
    fs::rename(
        env.notes.join("index.md"),
        env.notes.join(".temporarily-missing"),
    )
    .unwrap();
    // Without the old grace period, a missing page leaves the index promptly.
    wait_for_pages_absent(&env, "\tindex.md\t");
    fs::rename(
        env.notes.join(".temporarily-missing"),
        env.notes.join("index.md"),
    )
    .unwrap();
    fs::write(
        env.notes.join("index.md"),
        "---\nid: 424242\n---\n# Restored\n",
    )
    .unwrap();
    wait_for_pages(&env, "Restored");
    let pages = stdout(&env.run(&["index", "pages"]));
    assert!(pages
        .lines()
        .any(|line| { line.starts_with("424242\t") && line.contains("\tindex.md\tRestored") }));
}

#[test]
fn watch_tracks_git_checkout() {
    let env = TestEnv::new();
    git(&env.project, &["init", "-b", "main"]);
    fs::write(env.notes.join("index.md"), "# First branch\n").unwrap();
    git(&env.project, &["add", "notes"]);
    git(&env.project, &["commit", "-m", "first"]);
    git(&env.project, &["switch", "-c", "second"]);
    fs::write(env.notes.join("index.md"), "# Second branch\n").unwrap();
    git(&env.project, &["add", "notes/index.md"]);
    git(&env.project, &["commit", "-m", "second"]);
    git(&env.project, &["switch", "main"]);
    assert_success(&env.mount("Notes"));
    fs::write(env.notes.join("ready.md"), "# Watch ready\n").unwrap();
    wait_for_pages(&env, "Watch ready");
    git(&env.project, &["switch", "second"]);
    wait_for_pages(&env, "Second branch");
    git(&env.project, &["switch", "main"]);
    wait_for_pages(&env, "First branch");
}

#[test]
fn watch_reindexes_at_start_and_after_atomic_save() {
    let env = TestEnv::new();
    assert_success(&env.mount("Notes"));
    fs::write(env.notes.join("index.md"), "# Startup change\n").unwrap();
    wait_for_pages(&env, "Startup change");

    fs::write(env.notes.join(".new.md"), "# Atomic change\n").unwrap();
    fs::rename(env.notes.join(".new.md"), env.notes.join("index.md")).unwrap();
    wait_for_pages(&env, "Atomic change");

    let other = env.project.join("other");
    fs::create_dir(&other).unwrap();
    fs::write(other.join("other.md"), "# Other mount\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let output = env.run(&[
            "mount",
            "add",
            "-p",
            env.project.to_str().unwrap(),
            "-d",
            "other",
        ]);
        if output.status.success() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "mount failed: {}",
            stderr(&output)
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    fs::write(other.join("other.md"), "# Watched other mount\n").unwrap();
    wait_for_pages(&env, "Watched other mount");
    fs::remove_file(other.join("other.md")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let output = env.run(&["index", "pages"]);
        if output.status.success() && !stdout(&output).contains("Watched other mount") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "watch did not remove the deleted page"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn resolves_percent_encoded_markdown_paths_without_escaping_mount() {
    let env = TestEnv::new();
    fs::write(env.notes.join("source.md"), "---\nid: 100\n---\n[space](./a%20b.md#part) [japanese](%E6%97%A5%E6%9C%AC.md) [hash](hash%23name.md) [escape](%2E%2E/outside.md) [bad](bad%ZZ.md)\n").unwrap();
    fs::write(env.notes.join("a b.md"), "---\nid: 201\n---\n").unwrap();
    fs::write(env.notes.join("日本.md"), "---\nid: 202\n---\n").unwrap();
    fs::write(env.notes.join("hash#name.md"), "---\nid: 203\n---\n").unwrap();
    assert_success(&env.mount("Notes"));
    let output = env.run(&["index", "references", "100"]);
    assert_success(&output);
    let links = stdout(&output);
    assert!(links.contains("201\t./a%20b.md#part"), "{links}");
    assert!(links.contains("202\t%E6%97%A5%E6%9C%AC.md"), "{links}");
    assert!(links.contains("203\thash%23name.md"), "{links}");
    assert!(links.contains("-\t%2E%2E/outside.md"), "{links}");
    assert!(links.contains("-\tbad%ZZ.md"), "{links}");
}

#[test]
fn references_and_backlinks_follow_rescans() {
    let env = TestEnv::new();
    fs::write(env.notes.join("source.md"), "---\nid: 100\n---\n[Target](./target.md#heading) [[target]] [Web](https://example.com) [[Missing]]\n").unwrap();
    fs::write(env.notes.join("target.md"), "---\nid: 200\n---\n# Target\n").unwrap();
    assert_success(&env.mount("Notes"));
    let links = env.run(&["index", "references", "100"]);
    assert_success(&links);
    let links = stdout(&links);
    assert!(
        links.contains("200\t./target.md#heading\tmarkdown"),
        "{links}"
    );
    assert!(links.contains("200\ttarget\twiki"), "{links}");
    assert!(
        links.contains("-\thttps://example.com\tmarkdown"),
        "{links}"
    );
    assert!(links.contains("-\tMissing\twiki"), "{links}");
    let backlinks = env.run(&["index", "backlinks", "200"]);
    assert_success(&backlinks);
    assert!(stdout(&backlinks).contains("100\tsource.md"));

    fs::remove_file(env.notes.join("target.md")).unwrap();
    assert_success(&env.run(&["mount", "sync", "--all"]));
    let links = stdout(&env.run(&["index", "references", "100"]));
    assert!(links.contains("-\t./target.md#heading"), "{links}");
    let backlinks = env.run(&["index", "backlinks", "200"]);
    assert_success(&backlinks);
    assert!(stdout(&backlinks).contains("No links found."));
    assert!(!stdout(&env.run(&["index", "pages"])).contains("\ttarget.md\t"));
}

#[test]
fn wiki_links_remain_unresolved_when_ambiguous_then_resolve() {
    let env = TestEnv::new();
    fs::write(
        env.notes.join("source.md"),
        "---\nid: 100\n---\n[[Shared]]\n",
    )
    .unwrap();
    fs::write(env.notes.join("a.md"), "---\nid: 201\ntitle: Shared\n---\n").unwrap();
    fs::write(env.notes.join("b.md"), "---\nid: 202\ntitle: Shared\n---\n").unwrap();
    assert_success(&env.mount("Notes"));
    assert!(stdout(&env.run(&["index", "references", "100"])).contains("-\tShared\twiki"));
    fs::remove_file(env.notes.join("b.md")).unwrap();
    assert_success(&env.run(&["mount", "sync", "--all"]));
    assert!(stdout(&env.run(&["index", "references", "100"])).contains("201\tShared\twiki"));
}

#[test]
fn searches_japanese_and_tracks_index_changes() {
    let env = TestEnv::new();
    fs::write(
        env.notes.join("japanese.md"),
        "# 設計メモ\n\n日本語の検索テストです。\n",
    )
    .unwrap();
    assert_success(&env.mount("Notes"));

    for query in ["日本語", "検索", "語", "設計メモ"] {
        let output = env.run(&["index", "search", query]);
        assert_success(&output);
        assert!(stdout(&output).contains("japanese.md"));
    }
    assert!(stdout(&env.run(&["index", "search", "存在しない"])).contains("No matching pages"));
    fs::write(env.notes.join("japanese.md"), "# 新しい内容\n").unwrap();
    assert_success(&env.run(&["mount", "sync", "--all"]));
    assert!(stdout(&env.run(&["index", "search", "日本語"])).contains("No matching pages"));
    assert!(stdout(&env.run(&["index", "search", "新しい"])).contains("japanese.md"));
    fs::remove_file(env.notes.join("japanese.md")).unwrap();
    assert_success(&env.run(&["mount", "sync", "--all"]));
    assert!(stdout(&env.run(&["index", "search", "新しい"])).contains("No matching pages"));
    let mount_id = env.load_config().mounts[0].id.to_string();
    assert_success(&env.run(&["mount", "remove", &mount_id]));
    assert!(stdout(&env.run(&["index", "search", "architecture"])).contains("No matching pages"));
}

#[test]
fn search_treats_query_as_literal_and_validates_mount() {
    let env = TestEnv::new();
    fs::write(
        env.notes.join("literal.md"),
        "# Test\nA \"quoted\" value and 100%\n",
    )
    .unwrap();
    assert_success(&env.mount("Notes"));
    for query in ["\"quoted\"", "100%", "QUOTED"] {
        let output = env.run(&["index", "search", query]);
        assert_success(&output);
        assert!(stdout(&output).contains("literal.md"), "query: {query}");
    }
    fs::write(env.notes.join("other.md"), "# Other\nA quoted value\n").unwrap();
    let filtered = env.run(&["index", "search", "quoted", "--file", "literal"]);
    assert_success(&filtered);
    assert!(stdout(&filtered).contains("literal.md"));
    assert!(!stdout(&filtered).contains("other.md"));
    let regex = env.run(&[
        "index", "search", "qu[o]ted", "--regex", "--file", "literal",
    ]);
    assert_success(&regex);
    assert!(stdout(&regex).contains("literal.md"));
    assert_failure(&env.run(&["index", "search", ""]));
    assert_failure(&env.run(&["index", "search", "test", "--mount", "999"]));
}

#[test]
#[cfg(unix)]
fn restricts_existing_data_directory_and_database_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let env = TestEnv::new();
    let config = env.config_home.join("mindmount");
    let data = env.temp.path().join("data/mindmount");
    let state = env.temp.path().join("state/mindmount");
    fs::create_dir_all(&config).unwrap();
    fs::create_dir_all(&data).unwrap();
    fs::create_dir_all(&state).unwrap();
    for path in [&config, &data, &state] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert_success(&env.mount("Notes"));
    fs::set_permissions(env.database_path(), fs::Permissions::from_mode(0o644)).unwrap();
    let _ = env.stop_daemon();
    assert_success(&env.run(&["mount", "list"]));
    for path in [&config, &data, &state, &env.database_path()] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o077,
            0,
            "{}",
            path.display()
        );
    }
    assert!(stdout(&env.run(&["index", "search", "Architecture"])).contains("architecture.md"));
}

#[test]
fn mount_lists_pages_and_unmount_leaves_files_untouched() {
    let env = TestEnv::new();
    let before = snapshot(&env.notes);

    let output = env.mount("Notes");
    assert_success(&output);
    assert!(stdout(&output).contains("Mounted \"Notes\""));
    assert!(stdout(&output).contains("3 files"));

    assert!(env.config_path().is_file());
    assert!(env.database_path().is_file());
    assert_eq!(snapshot(&env.notes), before);

    let config = env.load_config();
    assert_eq!(config.mounts.len(), 1);
    let mount = &config.mounts[0];
    assert_eq!(mount.name, "Notes");
    assert_eq!(mount.directory_path, env.notes.canonicalize().unwrap());

    let output = env.run(&["index", "pages"]);
    assert_success(&output);
    let pages = stdout(&output);
    assert!(pages.contains("index.md"));
    assert!(pages.contains("architecture.md"));
    assert!(pages.contains("decisions/storage.md"));
    assert!(pages.contains("Architecture"));

    let output = env.run(&["mount", "list"]);
    assert_success(&output);
    let mounts = stdout(&output);
    assert!(mounts.contains(&mount.id.to_string()));
    assert!(mounts.contains("Notes"));

    let output = env.run(&["mount", "remove", &mount.id.to_string()]);
    assert_success(&output);
    assert!(stdout(&output).contains("left untouched"));
    assert!(env.load_config().mounts.is_empty());
    assert_eq!(snapshot(&env.notes), before);

    let output = env.run(&["mount", "list"]);
    assert_success(&output);
    assert!(stdout(&output).contains("No mounts registered."));

    let output = env.run(&["mount", "list", "--all"]);
    assert_success(&output);
    assert!(stdout(&output).contains("Notes"));
    assert!(stdout(&output).contains("\tno\t"));

    let output = env.run(&["index", "pages"]);
    assert_success(&output);
    assert!(stdout(&output).contains("No pages indexed."));
}

#[test]
fn mounting_the_same_directory_twice_is_idempotent() {
    let env = TestEnv::new();

    let first = env.mount("Notes");
    assert_success(&first);
    let first_config = env.load_config();
    let first_id = first_config.mounts[0].id;

    let second = env.mount("Notes again");
    assert_success(&second);
    assert!(stdout(&second).contains("already registered"));

    let second_config = env.load_config();
    assert_eq!(second_config.mounts.len(), 1);
    assert_eq!(second_config.mounts[0].id, first_id);
    assert_eq!(second_config.mounts[0].name, "Notes");
    assert_eq!(second_config.node_id, first_config.node_id);
}

#[test]
fn defaults_the_mount_name_to_the_directory_name() {
    let env = TestEnv::new();

    let output = env.run(&[
        "mount",
        "add",
        "-p",
        env.project.to_str().unwrap(),
        "-d",
        "./notes",
    ]);
    assert_success(&output);
    assert_eq!(env.load_config().mounts[0].name, "notes");
}

#[test]
fn rejects_nested_mounts() {
    let env = TestEnv::new();
    assert_success(&env.mount("Notes"));

    let output = env.run(&[
        "mount",
        "add",
        "-p",
        env.project.to_str().unwrap(),
        "-d",
        "./notes/decisions",
        "-n",
        "Decisions",
    ]);
    assert_failure(&output);
    assert!(stderr(&output).contains("overlaps with existing mount"));
    assert_eq!(env.load_config().mounts.len(), 1);
}

#[test]
fn rejects_directories_outside_the_project() {
    let env = TestEnv::new();
    let outside = env.temp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();

    let output = env.run(&[
        "mount",
        "add",
        "-p",
        env.project.to_str().unwrap(),
        "-d",
        outside.to_str().unwrap(),
    ]);
    assert_failure(&output);
    assert!(stderr(&output).contains("must be inside the project"));
    assert!(env.load_config().mounts.is_empty());
}

#[test]
fn rejects_missing_directories() {
    let env = TestEnv::new();

    let output = env.run(&[
        "mount",
        "add",
        "-p",
        env.project.to_str().unwrap(),
        "-d",
        "./missing",
    ]);
    assert_failure(&output);
    assert!(stderr(&output).contains("cannot access directory"));
    assert!(env.load_config().mounts.is_empty());
}

#[test]
fn reindex_rebuilds_a_deleted_database() {
    let env = TestEnv::new();
    assert_success(&env.mount("Notes"));
    let _ = env.stop_daemon();

    for suffix in ["", "-wal", "-shm"] {
        let path = PathBuf::from(format!("{}{suffix}", env.database_path().display()));
        if path.exists() {
            fs::remove_file(path).unwrap();
        }
    }
    assert!(!env.database_path().exists());

    let output = env.run(&["mount", "sync", "--all"]);
    assert_success(&output);
    assert!(stdout(&output).contains("3 files"));
    assert!(env.database_path().is_file());

    let output = env.run(&["index", "pages"]);
    assert_success(&output);
    assert!(stdout(&output).contains("decisions/storage.md"));
}

#[test]
fn root_invocation_mounts_and_notes_the_phase_four_server() {
    let env = TestEnv::new();

    let output = env.run(&[
        "-p",
        env.project.to_str().unwrap(),
        "-d",
        "./notes",
        "-n",
        "Root note",
    ]);
    assert_success(&output);
    assert!(stdout(&output).contains("Mounted \"Root note\""));
    assert!(stdout(&output).contains("mindmount daemon start"));
    assert_eq!(env.load_config().mounts.len(), 1);
}

#[test]
fn without_arguments_prints_help() {
    let env = TestEnv::new();
    let output = env.run(&[]);

    assert_success(&output);
    assert!(stdout(&output).contains("Usage: mindmount"));
    assert!(!env.config_path().exists());
}

#[test]
fn refuses_concurrent_operations_without_modifying_configuration() {
    let env = TestEnv::new();
    assert_success(&env.mount("Notes"));
    let output = env.run(&["daemon", "start", "--port", "0"]);
    assert_failure(&output);
    assert!(stderr(&output).contains("already running"));
}

#[test]
fn cli_fails_cleanly_when_the_daemon_is_stopped() {
    let env = TestEnv::new();
    let output = env.run_native(&["mount", "list"]);
    assert_failure(&output);
    assert!(
        stderr(&output).contains("Mindmount daemon is not running"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn daemon_start_rejects_non_loopback_hosts() {
    let env = TestEnv::new();

    let output = env.run(&["daemon", "start", "--host", "0.0.0.0"]);
    assert_failure(&output);
    assert!(stderr(&output).contains("loopback"));
}

#[test]
fn daemon_run_serves_mounts() {
    let env = TestEnv::new();
    assert_success(&env.mount("Notes"));
    let _ = env.stop_daemon();

    let port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut child = env
        .command()
        .args(["daemon", "run", "--port", &port.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "daemon did not start");
        std::thread::sleep(Duration::from_millis(100));
    }

    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        stream,
        "GET /mounts HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();

    let _ = child.kill();
    let _ = child.wait();

    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("Notes"), "{response}");
}

fn http_request(port: u16, request: &str) -> String {
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(stream, "{request}").unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

fn get_until_ready(port: u16, path: &str, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let request =
            format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            let response = http_request(port, &request);
            if response.contains(expected) {
                return;
            }
        }
        assert!(Instant::now() < deadline, "daemon did not serve {path}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn daemon_serves_web_and_mcp() {
    let env = TestEnv::new();
    assert_success(&env.mount("Notes"));
    let port = env.port();

    get_until_ready(port, "/mounts", "Notes");
    let status = env.run(&["daemon", "status"]);
    assert_success(&status);
    assert!(stdout(&status).contains("running"), "{}", stdout(&status));

    let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#;
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let unauthorized = http_request(port, &request);
    assert!(unauthorized.starts_with("HTTP/1.1 401"), "{unauthorized}");

    let api_request = format!(
        "GET /api/admin/status HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    );
    let unauthorized = http_request(port, &api_request);
    assert!(unauthorized.starts_with("HTTP/1.1 401"), "{unauthorized}");

    let credentials =
        Credentials::load(&env.config_home.join("mindmount/credentials.toml")).unwrap();
    let authorized = format!(
        "GET /api/admin/status HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n",
        credentials.mcp_token
    );
    let response = http_request(port, &authorized);
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("\"status\":\"ok\""), "{response}");

    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        credentials.mcp_token,
        body.len()
    );
    let response = http_request(port, &request);
    assert!(
        response.contains("protocolVersion") || response.contains("event:"),
        "{response}"
    );

    let stop = env.stop_daemon().expect("test daemon was running");
    assert_success(&stop);
    assert!(stdout(&stop).contains("Stopped"), "{}", stdout(&stop));
}

#[test]
fn metadata_and_knowledge_links_round_trip() {
    let env = TestEnv::new();
    fs::write(env.notes.join("index.md"), "---\nid: 100\n---\n# Index\n").unwrap();
    fs::write(env.notes.join("target.md"), "---\nid: 200\n---\n# Target\n").unwrap();
    assert_success(&env.mount("Notes"));

    let set = env.run(&["metadata", "set", "100", "owner", "\"me\""]);
    assert_success(&set);
    let metadata = env.run(&["metadata", "get", "100"]);
    assert_success(&metadata);
    assert!(stdout(&metadata).contains("owner"), "{}", stdout(&metadata));
    assert!(
        stdout(&metadata).contains("\"me\""),
        "{}",
        stdout(&metadata)
    );

    let add = env.run(&[
        "link",
        "add",
        "100",
        "--target",
        "200",
        "--relation",
        "depends_on",
        "--status",
        "confirmed",
    ]);
    assert_success(&add);
    let link_id = stdout(&add).split_whitespace().last().unwrap().to_string();
    let links = env.run(&["index", "references", "100"]);
    assert_success(&links);
    assert!(
        stdout(&links).contains("200\t\tdepends_on"),
        "{}",
        stdout(&links)
    );

    let delete = env.run(&["link", "delete", &link_id]);
    assert_success(&delete);
    assert!(stdout(&delete).contains("Deleted"));
    let links = env.run(&["index", "references", "100"]);
    assert!(!stdout(&links).contains("depends_on"));
}

#[test]
fn backup_and_version_work() {
    let env = TestEnv::new();
    assert_success(&env.mount("Notes"));
    let backup = env.temp.path().join("backup.sqlite");

    let output = env.run(&["backup", backup.to_str().unwrap()]);
    assert_success(&output);
    assert!(backup.is_file());

    let output = env.run(&["backup", backup.to_str().unwrap()]);
    assert_failure(&output);

    let output = env.run(&["version"]);
    assert_success(&output);
    assert!(stdout(&output).contains("mindmount"));
}

#[test]
fn mount_sync_updates_a_single_mount() {
    let env = TestEnv::new();
    assert_success(&env.mount("Notes"));
    let mount_id = env.load_config().mounts[0].id.to_string();

    fs::write(env.notes.join("index.md"), "# Synced\n").unwrap();
    let output = env.run(&["mount", "sync", &mount_id]);
    assert_success(&output);
    assert!(stdout(&env.run(&["index", "pages"])).contains("Synced"));
}
