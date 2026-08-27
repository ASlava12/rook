use rook_core::{AuthoredSkill, Config, Rook};
use rook_skills::{Environment, Requirements, SkillIndex};
use rook_store::Store;

/// `write_skill` writes into the user skills directory, which `ROOK_HOME`
/// redirects — and that is one variable for the whole process. Set once here,
/// so tests running in parallel cannot take it from each other; they stay apart
/// by using distinct skill names instead.
fn home() -> &'static std::path::Path {
    static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let dir = HOME.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ROOK_HOME", dir.path()) };
        std::fs::create_dir_all(dir.path().join("skills")).unwrap();
        dir
    });
    dir.path()
}

struct Fixture {
    _workspace: tempfile::TempDir,
    rook: Rook,
}

fn fixture() -> Fixture {
    let workspace = tempfile::tempdir().unwrap();
    let store = Store::open(workspace.path().join("store")).unwrap();
    let env = Environment::bare("linux", "x86_64", "0.1.0").with_language("rust", "1.97.1");
    let (skills, _) = SkillIndex::discover(&[(home().join("skills"), rook_skills::SkillSource::User)]);
    let rook = Rook::from_parts(store, Config::default(), env, skills, workspace.path().to_path_buf());
    Fixture { _workspace: workspace, rook }
}

fn skill(name: &str, body: &str) -> AuthoredSkill {
    AuthoredSkill {
        name: name.into(),
        description: "Build this project reproducibly.".into(),
        body: body.into(),
        keywords: vec!["build".into()],
        requires: Requirements::default(),
    }
}

#[test]
fn a_written_skill_is_loadable_without_restarting() {
    let f = fixture();
    f.rook.write_skill(&skill("reproducible-build", "Run `cargo xtask ci`.")).unwrap();

    let resolved = f.rook.skills().resolve("reproducible-build", &f.rook.env).unwrap();
    assert!(resolved.body.contains("cargo xtask ci"));
    assert_eq!(resolved.skill.manifest.description, "Build this project reproducibly.");
    assert_eq!(resolved.skill.manifest.keywords, ["build"]);
}

#[test]
fn a_written_skill_is_captured_as_a_version() {
    let f = fixture();
    f.rook.write_skill(&skill("versioned", "First.")).unwrap();
    f.rook.write_skill(&skill("versioned", "Second, after learning more.")).unwrap();

    let history = f.rook.skill_history("versioned").unwrap();
    assert_eq!(history.len(), 2, "rewriting keeps the old version: {history:?}");
    assert!(f.rook.skills().resolve("versioned", &f.rook.env).unwrap().body.contains("Second"));
}

#[test]
fn requirements_travel_into_the_frontmatter_and_gate_the_skill() {
    let f = fixture();
    let mut authored = skill("bsd-only", "Use `sed -i ''`.");
    authored.requires = Requirements { os: vec!["freebsd".into()], ..Default::default() };
    f.rook.write_skill(&authored).unwrap();

    let err = f.rook.skills().resolve("bsd-only", &f.rook.env).unwrap_err().to_string();
    assert!(err.contains("freebsd"), "a skill that claims freebsd must not fire on linux: {err}");

    let card = f.rook.catalog().into_iter().find(|c| c.name == "bsd-only").unwrap();
    assert!(!card.applicable, "and the catalog must say so rather than advertising it");
}

#[test]
fn a_name_that_is_not_a_directory_name_is_refused() {
    let f = fixture();
    for bad in ["../escape", "with space", "", "Upper/Case"] {
        let err = f.rook.write_skill(&skill(bad, "x")).unwrap_err().to_string();
        assert!(err.contains("not a usable skill name"), "{bad:?} was accepted: {err}");
    }
}

#[test]
fn a_skill_that_would_not_load_is_reported_rather_than_left_broken() {
    let f = fixture();
    let mut authored = skill("broken", "body");
    authored.requires = Requirements {
        language: [("rust".to_string(), "not a version".to_string())].into(),
        ..Default::default()
    };

    let err = f.rook.write_skill(&authored).unwrap_err().to_string();
    assert!(err.contains("does not load"), "{err}");
}
