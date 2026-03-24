use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn binary_path() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("target/debug/ai-workspace");
    path
}

fn temp_db() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("workspace.db");
    (dir, db_path)
}

fn run_cmd_in_dir(
    db_path: &PathBuf,
    dir: &std::path::Path,
    args: &[&str],
) -> (String, String, bool) {
    let output = Command::new(binary_path())
        .args(args)
        .current_dir(dir)
        .env("AI_WORKSPACE_DB", db_path.to_string_lossy().to_string())
        .env("RUST_LOG", "debug")
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (stdout, stderr, output.status.success())
}

#[test]
fn test_init_creates_project() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    let (stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "test-project"],
    );
    assert!(success, "init should succeed");
    assert!(stdout.contains("Initialized project 'test-project'"));
}

#[test]
fn test_init_with_group() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    let (stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "my-proj", "--group", "my-group"],
    );
    assert!(success, "init with group should succeed");
    assert!(stdout.contains("Initialized project 'my-proj'"));
    assert!(stdout.contains("Joined group 'my-group'"));
}

#[test]
fn test_init_idempotent() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);
    let (stdout, _stderr, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);
    assert!(success, "second init should succeed");
    assert!(stdout.contains("already initialized"));
}

#[test]
fn test_share_file() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::write(project_dir.path().join("hello.txt"), "hello world").unwrap();
    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);

    let (stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["share", "hello.txt", "--label", "greeting"],
    );
    assert!(success, "share should succeed");
    assert!(stdout.contains("Shared 'hello.txt'"));
}

#[test]
fn test_share_directory() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::create_dir(project_dir.path().join("docs")).unwrap();
    fs::write(project_dir.path().join("docs/README.md"), "# Docs").unwrap();
    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);

    let (stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["share", "docs", "--label", "documentation"],
    );
    assert!(success, "share dir should succeed");
    assert!(stdout.contains("Shared dir 'docs'"));
}

#[test]
fn test_share_nonexistent_file() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);

    let (_stdout, _stderr, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["share", "nonexistent.txt"]);
    assert!(!success, "sharing nonexistent file should fail");
}

#[test]
fn test_note_group_scope() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--group", "g1"],
    );

    let (stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &[
            "note",
            "--group",
            "g1",
            "--label",
            "deploy-rule",
            "Always deploy on Fridays",
        ],
    );
    assert!(success, "group note should succeed");
    assert!(stdout.contains("Added note"));
    assert!(stdout.contains("to group 'g1'"));
}

#[test]
fn test_note_project_scope() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--group", "g1"],
    );

    let (stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &[
            "note",
            "--scope",
            "project",
            "--label",
            "api-note",
            "This project exposes REST API",
        ],
    );
    assert!(success, "project note should succeed");
    assert!(stdout.contains("Added project note"));
}

#[test]
fn test_rm_by_label() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::write(project_dir.path().join("config.toml"), "[settings]").unwrap();
    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);
    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["share", "config.toml", "--label", "config"],
    );

    let (stdout, _stderr, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["rm", "config"]);
    assert!(success, "rm by label should succeed");
    assert!(stdout.contains("Removed item with label 'config'"));
}

#[test]
fn test_rm_by_path() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::write(project_dir.path().join("data.json"), "{}").unwrap();
    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);
    run_cmd_in_dir(&db_path, project_dir.path(), &["share", "data.json"]);

    let (stdout, _stderr, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["rm", "data.json"]);
    assert!(success, "rm by path should succeed");
    assert!(stdout.contains("Removed item with path 'data.json'"));
}

#[test]
fn test_status() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--group", "g1"],
    );

    let (stdout, _stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(success, "status should succeed");
    assert!(stdout.contains("Project: proj"));
    assert!(stdout.contains("g1"));
}

#[test]
fn test_sync() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::write(project_dir.path().join("temp.txt"), "temp").unwrap();
    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);
    run_cmd_in_dir(&db_path, project_dir.path(), &["share", "temp.txt"]);

    fs::remove_file(project_dir.path().join("temp.txt")).unwrap();

    let (stdout, _stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["sync"]);
    assert!(success, "sync should succeed");
    assert!(stdout.contains("Removed 1 stale entries"));
}

// --- List ---

#[test]
fn test_list_empty() {
    let (_db_dir, db_path) = temp_db();
    let dir = tempfile::tempdir().unwrap();

    let (stdout, _stderr, success) = run_cmd_in_dir(&db_path, dir.path(), &["list"]);
    assert!(success, "list on empty workspace should succeed");
    assert!(stdout.contains("Projects: (none)"));
    assert!(stdout.contains("Groups: (none)"));
}

#[test]
fn test_list_projects_and_groups() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj-a", "--group", "team-x"],
    );

    let (stdout, _stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["list"]);
    assert!(success, "list should succeed");
    assert!(stdout.contains("proj-a"));
    assert!(stdout.contains("team-x"));
}

#[test]
fn test_list_projects_only() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj-b", "--group", "grp"],
    );

    let (stdout, _stderr, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["list", "projects"]);
    assert!(success, "list projects should succeed");
    assert!(stdout.contains("proj-b"));
    assert!(!stdout.contains("Groups:"));
}

#[test]
fn test_list_groups_only() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj-c", "--group", "grp-z"],
    );

    let (stdout, _stderr, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["list", "groups"]);
    assert!(success, "list groups should succeed");
    assert!(stdout.contains("grp-z"));
    assert!(!stdout.contains("Projects:"));
}

#[test]
fn test_list_no_project_required() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();
    let other_dir = tempfile::tempdir().unwrap();

    // Init project in one dir
    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "remote-proj"],
    );

    // List from a different dir (not a project)
    let (stdout, _stderr, success) = run_cmd_in_dir(&db_path, other_dir.path(), &["list"]);
    assert!(success, "list should work outside project dir");
    assert!(stdout.contains("remote-proj"));
}

// --- Edit ---

#[test]
fn test_edit_note_content() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--group", "g1"],
    );
    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["note", "--scope", "project", "--label", "info", "old text"],
    );

    let (stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["edit", "info", "--content", "new text"],
    );
    assert!(success, "edit content should succeed");
    assert!(stdout.contains("Updated item"));

    // Verify via status
    let (stdout, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(stdout.contains("new text"));
}

#[test]
fn test_edit_note_label() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--group", "g1"],
    );
    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &[
            "note",
            "--scope",
            "project",
            "--label",
            "old-label",
            "content",
        ],
    );

    let (stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["edit", "old-label", "--label", "new-label"],
    );
    assert!(success, "edit label should succeed");
    assert!(stdout.contains("Updated item"));

    let (stdout, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(stdout.contains("new-label"));
}

#[test]
fn test_edit_scope_project_to_group() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--group", "g1"],
    );
    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &[
            "note",
            "--scope",
            "project",
            "--label",
            "my-note",
            "scope test",
        ],
    );

    let (stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["edit", "my-note", "--scope", "group", "--group", "g1"],
    );
    assert!(success, "edit scope to group should succeed");
    assert!(stdout.contains("Updated item"));

    // Verify: note should now appear in group notes section of status
    let (stdout, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(stdout.contains("Group 'g1' shared items:"));
    assert!(stdout.contains("scope test"));
}

#[test]
fn test_edit_scope_group_to_project() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--group", "g1"],
    );
    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &[
            "note",
            "--scope",
            "group",
            "--group",
            "g1",
            "--label",
            "grp-note",
            "group text",
        ],
    );

    let (stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["edit", "grp-note", "--scope", "project"],
    );
    assert!(success, "edit scope to project should succeed");
    assert!(stdout.contains("Updated item"));

    // Verify: note should appear in shared items, not group notes
    let (stdout, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(stdout.contains("group text"));
    assert!(!stdout.contains("Group notes in 'g1'"));
}

#[test]
fn test_edit_no_flags_fails() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);
    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["note", "--scope", "project", "--label", "lbl", "text"],
    );

    let (_stdout, _stderr, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["edit", "lbl"]);
    assert!(!success, "edit with no flags should fail");
}

#[test]
fn test_edit_nonexistent_fails() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);

    let (_stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["edit", "ghost", "--label", "x"],
    );
    assert!(!success, "edit nonexistent should fail");
}

// --- Leave & Delete Group ---

#[test]
fn test_leave_group() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--group", "g1"],
    );

    let (stdout, _stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["leave", "g1"]);
    assert!(success, "leave should succeed");
    assert!(stdout.contains("Left group 'g1'"));
    assert!(
        stdout.contains("was deleted"),
        "group with no members should be auto-deleted"
    );

    // Verify group is gone from status
    let (stdout, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(stdout.contains("(none)"));
}

#[test]
fn test_leave_group_not_member() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--group", "g1"],
    );

    // Leave first time — group gets auto-deleted (no members left)
    run_cmd_in_dir(&db_path, project_dir.path(), &["leave", "g1"]);

    // Leave again — group no longer exists
    let (_stdout, _stderr, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["leave", "g1"]);
    assert!(!success, "leave should fail because group was auto-deleted");
}

#[test]
fn test_leave_nonexistent_group() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);

    let (_stdout, _stderr, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["leave", "nope"]);
    assert!(!success, "leave nonexistent group should fail");
}

#[test]
fn test_delete_group() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--group", "g1"],
    );
    // Add a group note
    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["note", "--group", "g1", "group note"],
    );

    let (stdout, _stderr, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["delete-group", "g1"]);
    assert!(success, "delete-group should succeed");
    assert!(stdout.contains("Deleted group 'g1'"));

    // Verify group is gone from status
    let (stdout, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(!stdout.contains("g1"));
}

#[test]
fn test_delete_nonexistent_group() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);

    let (_stdout, _stderr, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["delete-group", "nope"]);
    assert!(!success, "delete nonexistent group should fail");
}

// --- Export & Config ---

#[test]
fn test_export_creates_json() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::write(project_dir.path().join("readme.md"), "# Hello").unwrap();
    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "my-proj", "--group", "team"],
    );
    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["share", "readme.md", "--label", "docs"],
    );
    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &[
            "note",
            "--scope",
            "project",
            "--label",
            "info",
            "important note",
        ],
    );

    let (stdout, _stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["export"]);
    assert!(success, "export should succeed");
    assert!(stdout.contains("Exported config"));

    let config_path = project_dir.path().join(".ai-workspace.json");
    assert!(config_path.exists(), ".ai-workspace.json should be created");

    let content = fs::read_to_string(&config_path).unwrap();
    assert!(content.contains("\"my-proj\""));
    assert!(content.contains("team"));
    assert!(content.contains("readme.md"));
    assert!(content.contains("docs"));
    assert!(content.contains("important note"));
}

#[test]
fn test_init_reads_json() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    // Write .ai-workspace.json before init
    let config = r#"{
        "name": "from-json",
        "groups": ["team-a"],
        "share": ["README.md"],
        "notes": [{"content": "hello from json", "label": "greeting"}]
    }"#;
    fs::write(project_dir.path().join(".ai-workspace.json"), config).unwrap();
    fs::write(project_dir.path().join("README.md"), "# Readme").unwrap();

    let (stdout, _stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["init"]);
    assert!(success, "init with json should succeed");
    assert!(stdout.contains("from-json"));

    // Verify state via status
    let (stdout, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(stdout.contains("from-json"));
    assert!(stdout.contains("team-a"));
    assert!(stdout.contains("greeting"));
}

#[test]
fn test_init_name_flag_overrides_json() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    let config = r#"{"name": "json-name", "groups": []}"#;
    fs::write(project_dir.path().join(".ai-workspace.json"), config).unwrap();

    let (stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "cli-name"],
    );
    assert!(success);
    assert!(stdout.contains("cli-name"));

    let (stdout, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(stdout.contains("cli-name"));
}

#[test]
fn test_share_updates_json() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::write(project_dir.path().join("a.txt"), "a").unwrap();
    fs::write(project_dir.path().join("b.txt"), "b").unwrap();

    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);
    run_cmd_in_dir(&db_path, project_dir.path(), &["share", "a.txt"]);

    // Export to create .json
    run_cmd_in_dir(&db_path, project_dir.path(), &["export"]);

    // Share another file — should update .json
    run_cmd_in_dir(&db_path, project_dir.path(), &["share", "b.txt"]);

    let content = fs::read_to_string(project_dir.path().join(".ai-workspace.json")).unwrap();
    assert!(
        content.contains("b.txt"),
        ".json should be updated with new share"
    );
}

#[test]
fn test_group_note_does_not_update_json() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--group", "grp"],
    );

    // Export to create .json
    run_cmd_in_dir(&db_path, project_dir.path(), &["export"]);
    let before = fs::read_to_string(project_dir.path().join(".ai-workspace.json")).unwrap();

    // Add group note — should NOT update .json
    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["note", "--scope", "group", "group secret"],
    );

    let after = fs::read_to_string(project_dir.path().join(".ai-workspace.json")).unwrap();
    assert_eq!(before, after, "group note should not modify .json");
    assert!(!after.contains("group secret"));
}

#[test]
fn test_solo_dev_no_json() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::write(project_dir.path().join("file.txt"), "x").unwrap();

    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "solo"]);
    run_cmd_in_dir(&db_path, project_dir.path(), &["share", "file.txt"]);

    // No .ai-workspace.json should exist
    assert!(
        !project_dir.path().join(".ai-workspace.json").exists(),
        "solo dev without export should not have .json"
    );
}

#[test]
fn test_sync_with_json() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);

    // Write a config with shares and notes
    let config = r#"{
        "name": "proj",
        "groups": ["team"],
        "share": ["new-file.txt"],
        "notes": [{"content": "synced note", "label": "sync-lbl"}]
    }"#;
    fs::write(project_dir.path().join(".ai-workspace.json"), config).unwrap();

    let (stdout, _stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["sync"]);
    assert!(success, "sync with json should succeed");
    assert!(stdout.contains("Config sync:") || stdout.contains("Config is in sync"));

    // Verify via status
    let (stdout, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(stdout.contains("team"));
    assert!(stdout.contains("sync-lbl"));
}

// --- Auto-share on init ---

#[test]
fn test_auto_share_rust_project() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::write(
        project_dir.path().join("Cargo.toml"),
        "[package]\nname = \"test\"",
    )
    .unwrap();
    fs::write(project_dir.path().join("README.md"), "# Test").unwrap();

    let (stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "rust-proj"],
    );
    assert!(success, "init should succeed");
    assert!(stdout.contains("Auto-shared 2 key file(s)"));

    // Verify via status
    let (stdout, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(stdout.contains("Cargo.toml"));
    assert!(stdout.contains("README.md"));
}

#[test]
fn test_auto_share_node_project() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::write(
        project_dir.path().join("package.json"),
        "{\"name\": \"test\"}",
    )
    .unwrap();
    fs::write(project_dir.path().join("README.rst"), "Test").unwrap();

    let (stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "node-proj"],
    );
    assert!(success, "init should succeed");
    assert!(stdout.contains("Auto-shared 2 key file(s)"));

    let (stdout, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(stdout.contains("package.json"));
    assert!(stdout.contains("README.rst"));
}

#[test]
fn test_auto_share_skipped_when_json_exists() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::write(project_dir.path().join("Cargo.toml"), "[package]").unwrap();
    fs::write(project_dir.path().join("README.md"), "# Test").unwrap();
    fs::write(
        project_dir.path().join(".ai-workspace.json"),
        "{\"name\": \"proj\", \"groups\": [], \"share\": [], \"notes\": []}",
    )
    .unwrap();

    let (stdout, _stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["init"]);
    assert!(success, "init should succeed");
    // Should NOT auto-share when .ai-workspace.json exists
    assert!(!stdout.contains("Auto-shared"));

    let (stdout, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(!stdout.contains("Cargo.toml"));
}

#[test]
fn test_auto_share_no_duplicates_on_reinit() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::write(project_dir.path().join("Cargo.toml"), "[package]").unwrap();
    fs::write(project_dir.path().join("README.md"), "# Test").unwrap();

    // First init — auto-shares
    let (stdout, _stderr, _) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);
    assert!(stdout.contains("Auto-shared 2 key file(s)"));

    // Second init — should not auto-share again (already shared)
    let (stdout, _stderr, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);
    assert!(success, "re-init should succeed");
    assert!(!stdout.contains("Auto-shared"));

    // Verify only 2 shared items, not 4
    let (stdout, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    let cargo_count = stdout.matches("Cargo.toml").count();
    assert_eq!(cargo_count, 1, "Cargo.toml should appear exactly once");
}
