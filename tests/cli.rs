use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use mindmount::config::Config;
use tempfile::TempDir;

struct TestEnv {
    temp: TempDir,
    config_home: PathBuf,
    project: PathBuf,
    notes: PathBuf,
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
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mindmount"));
        command.env("XDG_CONFIG_HOME", &self.config_home);
        command.env("HOME", self.temp.path());
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command()
            .args(args)
            .output()
            .expect("failed to run mindmount")
    }

    fn config_path(&self) -> PathBuf {
        self.config_home.join("mindmount/config.toml")
    }

    fn database_path(&self) -> PathBuf {
        self.config_home.join("mindmount/db/mindmount.sqlite")
    }

    fn load_config(&self) -> Config {
        Config::load(&self.config_path()).unwrap()
    }

    fn mount(&self, name: &str) -> Output {
        self.run(&[
            "storage",
            "mount",
            "-p",
            self.project.to_str().unwrap(),
            "-d",
            "./notes",
            "-n",
            name,
        ])
    }
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
    assert_success(&env.run(&["index", "rescan"]));
    let links = stdout(&env.run(&["index", "references", "100"]));
    assert!(links.contains("-\t./target.md#heading"), "{links}");
    assert_failure(&env.run(&["index", "backlinks", "200"]));
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
    assert_success(&env.run(&["index", "rescan"]));
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
    assert_success(&env.run(&["index", "rescan"]));
    assert!(stdout(&env.run(&["index", "search", "日本語"])).contains("No matching pages"));
    assert!(stdout(&env.run(&["index", "search", "新しい"])).contains("japanese.md"));
    fs::remove_file(env.notes.join("japanese.md")).unwrap();
    assert_success(&env.run(&["index", "rescan"]));
    assert!(stdout(&env.run(&["index", "search", "新しい"])).contains("No matching pages"));
    let mount_id = env.load_config().mounts[0].id.to_string();
    assert_success(&env.run(&["storage", "unmount", &mount_id]));
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
    assert_failure(&env.run(&["index", "search", ""]));
    assert_failure(&env.run(&["index", "search", "test", "--mount", "999"]));
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

    let output = env.run(&["storage", "list"]);
    assert_success(&output);
    let mounts = stdout(&output);
    assert!(mounts.contains(&mount.id.to_string()));
    assert!(mounts.contains("Notes"));

    let output = env.run(&["storage", "unmount", &mount.id.to_string()]);
    assert_success(&output);
    assert!(stdout(&output).contains("left untouched"));
    assert!(env.load_config().mounts.is_empty());
    assert_eq!(snapshot(&env.notes), before);

    let output = env.run(&["storage", "list"]);
    assert_success(&output);
    assert!(stdout(&output).contains("No mounts registered."));

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
        "storage",
        "mount",
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
        "storage",
        "mount",
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
        "storage",
        "mount",
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
        "storage",
        "mount",
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

    for suffix in ["", "-wal", "-shm"] {
        let path = PathBuf::from(format!("{}{suffix}", env.database_path().display()));
        if path.exists() {
            fs::remove_file(path).unwrap();
        }
    }
    assert!(!env.database_path().exists());

    let output = env.run(&["index", "rescan"]);
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
    assert!(stdout(&output).contains("Phase 4"));
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
    let before = fs::read(env.config_path()).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(env.config_home.join("mindmount/mindmount.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let output = env.run(&["storage", "list"]);
    assert_failure(&output);
    assert!(stderr(&output).contains("another Mindmount process may be running"));
    assert_eq!(fs::read(env.config_path()).unwrap(), before);
    drop(lock);
    assert_success(&env.run(&["storage", "list"]));
}

#[test]
fn unimplemented_commands_report_their_phase() {
    let env = TestEnv::new();

    let output = env.run(&["serve"]);
    assert_failure(&output);
    assert!(stderr(&output).contains("Phase 4"));

    let output = env.run(&["mcp"]);
    assert_failure(&output);
    assert!(stderr(&output).contains("Phase 3"));
}
