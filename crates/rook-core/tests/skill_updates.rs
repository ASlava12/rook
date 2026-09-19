//! Updating the skills that came from a source, and leaving the rest alone.

use std::sync::{Mutex, MutexGuard};

use rook_core::{Refreshed, Rook};

static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
fn alone() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner())
}

fn card(name: &str, body: &str) -> String {
    format!("---\nname: {name}\ndescription: Use when {name} is wanted.\nversion: 1.0.0\n---\n\n{body}\n")
}

/// A directory is a source, which is what makes this testable without a network.
fn source_with(name: &str, body: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(name)).unwrap();
    std::fs::write(dir.path().join(name).join("SKILL.md"), card(name, body)).unwrap();
    dir
}

fn rook_with(source: &std::path::Path) -> (tempfile::TempDir, tempfile::TempDir, Rook) {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        format!("skill_sources = [{:?}]\n[agent]\ninstall_servers = false\n", source.display().to_string()),
    )
    .unwrap();
    unsafe { std::env::set_var("ROOK_HOME", home.path()) };
    let rook = Rook::open(Some(workspace.path().to_path_buf())).unwrap();
    (home, workspace, rook)
}

/// The ordinary case, and the one it is for.
#[test]
fn a_skill_from_a_source_is_brought_up_to_what_the_source_offers() {
    let _alone = alone();
    let source = source_with("greeting", "Say hello.");
    let (_home, _workspace, rook) = rook_with(source.path());
    rook.install_skill("greeting").unwrap();

    // The source moves on.
    std::fs::write(source.path().join("greeting/SKILL.md"), card("greeting", "Say hello, warmly.")).unwrap();

    let done = rook.update_skills().unwrap();
    assert!(matches!(done.as_slice(), [(name, Refreshed::Updated { .. })] if name == "greeting"), "{done:?}");
    let now = std::fs::read_to_string(rook_core::paths::user_skills_dir().join("greeting/SKILL.md")).unwrap();
    assert!(now.contains("warmly"), "the new body is on disk: {now}");

    // And again with nothing to do.
    let again = rook.update_skills().unwrap();
    assert!(matches!(again.as_slice(), [(_, Refreshed::Current)]), "{again:?}");
}

/// A skill somebody wrote here never came from a source and is not this
/// command's business.
#[test]
fn a_skill_written_here_is_left_alone_and_said_so() {
    let _alone = alone();
    let source = source_with("greeting", "Say hello.");
    let (_home, _workspace, rook) = rook_with(source.path());
    rook.new_skill("mine", "Use when mine is wanted.").unwrap();
    rook.reload_skills();

    let done = rook.update_skills().unwrap();
    assert!(
        done.iter().any(|(name, outcome)| name == "mine" && matches!(outcome, Refreshed::Yours)),
        "{done:?}"
    );
}

/// The one that matters most. An installed skill edited since is somebody's
/// work, and an update that discards it destroys the reason they installed it.
#[test]
fn an_installed_skill_edited_here_is_not_overwritten() {
    let _alone = alone();
    let source = source_with("greeting", "Say hello.");
    let (_home, _workspace, rook) = rook_with(source.path());
    rook.install_skill("greeting").unwrap();

    let here = rook_core::paths::user_skills_dir().join("greeting/SKILL.md");
    std::fs::write(&here, card("greeting", "Say hello, in the user's own language.")).unwrap();
    // And the source moves on as well, so there is something to overwrite with.
    std::fs::write(source.path().join("greeting/SKILL.md"), card("greeting", "Say hello, warmly.")).unwrap();

    let done = rook.update_skills().unwrap();
    assert!(matches!(done.as_slice(), [(name, Refreshed::Edited { .. })] if name == "greeting"), "{done:?}");
    let kept = std::fs::read_to_string(&here).unwrap();
    assert!(kept.contains("own language"), "the edit survived the update: {kept}");
    assert!(!kept.contains("warmly"), "and the source did not land on top of it");
}

/// The one that was wrong. What "edited since" is measured against has to be
/// the last version this command put there, not the first one ever installed —
/// otherwise every skill reads as edited the moment it has been updated once,
/// and the next update after that is held back for an edit nobody made.
#[test]
fn an_update_becomes_what_the_next_edit_is_measured_against() {
    let _alone = alone();
    let source = source_with("greeting", "Say hello.");
    let (_home, _workspace, rook) = rook_with(source.path());
    rook.install_skill("greeting").unwrap();

    std::fs::write(source.path().join("greeting/SKILL.md"), card("greeting", "Say hello, warmly.")).unwrap();
    let updated = rook.update_skills().unwrap();
    assert!(matches!(updated.as_slice(), [(_, Refreshed::Updated { .. })]), "{updated:?}");

    // Nothing has been edited here, and the source has moved again.
    std::fs::write(source.path().join("greeting/SKILL.md"), card("greeting", "Say hello, twice.")).unwrap();
    let again = rook.update_skills().unwrap();
    assert!(
        matches!(again.as_slice(), [(_, Refreshed::Updated { .. })]),
        "a skill updated once is not thereafter reported as somebody's edit: {again:?}"
    );
    let now = std::fs::read_to_string(rook_core::paths::user_skills_dir().join("greeting/SKILL.md")).unwrap();
    assert!(now.contains("twice"), "{now}");

    // And an edit after an update is still somebody's work.
    let here = rook_core::paths::user_skills_dir().join("greeting/SKILL.md");
    std::fs::write(&here, card("greeting", "Say hello, in the user's own language.")).unwrap();
    std::fs::write(source.path().join("greeting/SKILL.md"), card("greeting", "Say hello, three times."))
        .unwrap();
    let held = rook.update_skills().unwrap();
    assert!(matches!(held.as_slice(), [(_, Refreshed::Edited { .. })]), "{held:?}");
    assert!(std::fs::read_to_string(&here).unwrap().contains("own language"), "the edit survived");
}

/// Comparing a catalogue against what is installed is a question about content.
/// Answering it by capturing both put every version of every skill anybody ever
/// offered into the store, which is a growing store nobody asked for and a
/// growing GC bill behind it.
#[test]
fn checking_for_updates_does_not_store_a_copy_of_everything_a_source_offers() {
    let _alone = alone();
    let source = source_with("greeting", "Say hello.");
    let (_home, _workspace, rook) = rook_with(source.path());
    rook.install_skill("greeting").unwrap();

    // Something the source offers and nobody has installed.
    std::fs::create_dir_all(source.path().join("unwanted")).unwrap();
    std::fs::write(
        source.path().join("unwanted/SKILL.md"),
        card("unwanted", "A skill nobody here asked for."),
    )
    .unwrap();

    let before = rook.stats().unwrap().objects;
    let done = rook.update_skills().unwrap();
    assert!(matches!(done.as_slice(), [(_, Refreshed::Current)]), "{done:?}");
    let after = rook.stats().unwrap().objects;
    assert_eq!(after, before, "a check that changed nothing stored nothing: {before} → {after}");
}
