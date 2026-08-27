//! Getting a skill from somewhere else.
//!
//! A source is a git repository or a directory with skills in it: no index, no
//! API, nothing to agree on beyond the format everything here already speaks.
//! These use a directory, because a test that reaches the network tests the
//! network.

use rook_core::{Config, Rook};
use rook_skills::{Environment, SkillIndex};
use rook_store::Store;

fn source_with(dir: &std::path::Path, skills: &[(&str, &str)]) {
    for (name, description) in skills {
        let at = dir.join("skills").join(name);
        std::fs::create_dir_all(at.join("scripts")).unwrap();
        std::fs::write(
            at.join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: {description}\n---\n\n# {name}\n\nRun `scripts/go.sh`.\n"
            ),
        )
        .unwrap();
        std::fs::write(at.join("scripts/go.sh"), "#!/bin/sh\necho done\n").unwrap();
    }
}

fn rook_with_source(home: &std::path::Path, source: &std::path::Path) -> Rook {
    unsafe { std::env::set_var("ROOK_HOME", home) };
    let config = Config { skill_sources: vec![source.display().to_string()], ..Default::default() };
    let (skills, _) = SkillIndex::discover(&[]);
    Rook::from_parts(
        Store::open(home).unwrap(),
        config,
        Environment::bare("linux", "x86_64", "0.1.0"),
        skills,
        home.to_path_buf(),
    )
}

/// `ROOK_HOME` is process-wide, so these take turns.
static HOME: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn a_source_offers_what_it_has_and_a_query_ranks_it() {
    let _guard = HOME.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    source_with(
        source.path(),
        &[
            ("pdf-tools", "Read and write PDF files."),
            ("csv-tools", "Summarise a CSV column."),
            ("unrelated", "Something else entirely."),
        ],
    );
    let rook = rook_with_source(home.path(), source.path());

    let (all, errors) = rook.skills_offered("", false);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(all.len(), 3, "an empty query is everything they have");

    let (matched, _) = rook.skills_offered("pdf", false);
    assert_eq!(matched.first().map(|o| o.name.as_str()), Some("pdf-tools"), "{matched:?}");

    let (by_words, _) = rook.skills_offered("summarise a column", false);
    assert_eq!(by_words.first().map(|o| o.name.as_str()), Some("csv-tools"), "the description counts too");
}

/// The whole directory: `SKILL.md` alone is instructions for tools that are not
/// there, which is the failure the bundled-files work exists to prevent.
#[test]
fn installing_brings_the_files_the_instructions_call() {
    let _guard = HOME.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    source_with(source.path(), &[("pdf-tools", "Read and write PDF files.")]);
    let rook = rook_with_source(home.path(), source.path());

    let path = rook.install_skill("pdf-tools").unwrap();
    assert!(path.join("scripts/go.sh").exists(), "the script came with it");

    let card = rook.catalog().into_iter().find(|c| c.name == "pdf-tools").expect("it is a card now");
    assert_eq!(card.source, "user");
    assert!(rook.skills().resolve("pdf-tools", rook.env()).is_ok(), "and it loads");
}

#[test]
fn a_name_no_source_offers_says_what_is_close() {
    let _guard = HOME.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    source_with(source.path(), &[("pdf-tools", "Read and write PDF files.")]);
    let rook = rook_with_source(home.path(), source.path());

    let err = rook.install_skill("pdf").unwrap_err().to_string();
    assert!(err.contains("no source offers"), "{err}");
    assert!(err.contains("pdf-tools"), "and the closest one, since the name was nearly right: {err}");
}

#[test]
fn a_source_that_is_neither_a_directory_nor_a_repository_is_reported_not_ignored() {
    let _guard = HOME.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    let rook = rook_with_source(home.path(), std::path::Path::new("not-a-place"));

    let (offered, errors) = rook.skills_offered("", false);
    assert!(offered.is_empty());
    assert_eq!(errors.len(), 1, "a source that cannot be read is not a source with nothing in it");
}
