//! A plugin packages skills and MCP servers in one directory.
//!
//! `SkillSource::Plugin` was declared, ranked against the other sources and
//! given a label months before anything constructed it — an API advertising a
//! feature that was not there.

use std::path::Path;

use rook_core::plugins;

/// `ROOK_HOME` is process-wide, so the tests that need a plugin installed for
/// the *user* take turns; the workspace ones are free of it.
static HOME: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn skill(dir: &Path, name: &str) {
    write(
        &dir.join(format!("skills/{name}/SKILL.md")),
        &format!("---\nname: {name}\ndescription: A {name} skill.\nversion: 1.0.0\n---\nbody\n"),
    );
}

#[test]
fn a_plugin_brings_its_skills_and_its_servers() {
    let workspace = tempfile::tempdir().unwrap();
    let dir = workspace.path().join(".rook/plugins/rust-pack");
    write(
        &dir.join(".claude-plugin/plugin.json"),
        r#"{"name":"rust-pack","version":"1.2.0",
            "mcpServers":{"docs":{"command":"docs-server","args":["--stdio"]}}}"#,
    );
    skill(&dir, "tidy");

    let (plugins, errors) = plugins::discover(workspace.path());
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(plugins.len(), 1);

    let plugin = &plugins[0];
    assert_eq!(plugin.name, "rust-pack");
    assert_eq!(plugin.version, "1.2.0");
    assert_eq!(
        plugin.mcp[0].name, "rust-pack__docs",
        "namespaced, so two plugins shipping a `docs` server do not collide"
    );
    assert_eq!(plugin.mcp[0].command, "docs-server");
    assert_eq!(
        plugin.mcp[0].cwd.as_deref(),
        Some(dir.to_str().unwrap()),
        "a server ships with its plugin and runs there unless it says otherwise"
    );
    assert!(plugin.skills_dir().join("tidy/SKILL.md").is_file());
}

#[test]
fn a_manifest_without_a_name_is_named_by_its_directory() {
    let workspace = tempfile::tempdir().unwrap();
    write(&workspace.path().join(".rook/plugins/from-dir/plugin.json"), "{}");

    let (plugins, errors) = plugins::discover(workspace.path());
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(plugins[0].name, "from-dir", "the plain plugin.json is accepted too");
    assert_eq!(plugins[0].version, "0.0.0");
}

#[test]
fn servers_can_come_from_the_sidecar_the_ecosystem_writes() {
    let workspace = tempfile::tempdir().unwrap();
    let dir = workspace.path().join(".rook/plugins/sidecar");
    write(&dir.join("plugin.json"), r#"{"name":"sidecar"}"#);
    write(&dir.join(".mcp.json"), r#"{"mcpServers":{"fs":{"command":"server-filesystem"}}}"#);

    let (plugins, _) = plugins::discover(workspace.path());
    assert_eq!(plugins[0].mcp.len(), 1);
    assert_eq!(plugins[0].mcp[0].name, "sidecar__fs");
}

#[test]
fn a_directory_that_is_not_a_plugin_is_passed_over_rather_than_reported() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(workspace.path().join(".rook/plugins/notes")).unwrap();
    write(&workspace.path().join(".rook/plugins/notes/README.md"), "just files");

    let (plugins, errors) = plugins::discover(workspace.path());
    assert!(plugins.is_empty());
    assert!(errors.is_empty(), "the plugins directory is the user's and may hold anything: {errors:?}");
}

#[test]
fn a_manifest_that_does_not_parse_is_named_rather_than_skipped() {
    let workspace = tempfile::tempdir().unwrap();
    write(&workspace.path().join(".rook/plugins/broken/plugin.json"), "{ not json");

    let (plugins, errors) = plugins::discover(workspace.path());
    assert!(plugins.is_empty());
    assert_eq!(errors.len(), 1, "a shorter catalog with no explanation is the failure being avoided");
    assert!(errors[0].contains("broken"), "{errors:?}");
}

#[test]
fn a_project_skill_still_wins_over_the_one_a_plugin_ships() {
    let _guard = HOME.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("ROOK_HOME", home.path()) };

    let plugin = workspace.path().join(".rook/plugins/pack");
    write(&plugin.join("plugin.json"), r#"{"name":"pack"}"#);
    skill(&plugin, "shared");
    write(
        &workspace.path().join(".rook/skills/shared/SKILL.md"),
        "---\nname: shared\ndescription: The vendored one.\nversion: 0.0.1\n---\nproject\n",
    );

    let rook = rook_core::Rook::open(Some(workspace.path().to_path_buf())).unwrap();
    let resolved = rook.skills().resolve("shared", rook.env()).unwrap();
    assert_eq!(
        resolved.skill.source.label(),
        "project",
        "a skill vendored into the project is there on purpose, even against a newer version"
    );
    unsafe { std::env::remove_var("ROOK_HOME") };
}
