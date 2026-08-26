use std::path::{Path, PathBuf};

use rook_skills::{Environment, SkillError, SkillIndex, SkillSource};

fn write_skill(root: &Path, rel: &str, frontmatter: &str, body: &str) -> PathBuf {
    let dir = root.join(rel);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), format!("---\n{frontmatter}---\n{body}")).unwrap();
    dir
}

fn env_linux() -> Environment {
    Environment::bare("linux", "x86_64", "0.1.0").with_language("rust", "1.97.1").with_tool("git", "2.45.0")
}

#[test]
fn parses_a_plain_spec_skill() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_skill(
        dir.path(),
        "pdf",
        "name: pdf\ndescription: Fill in PDF forms.\n",
        "# PDF\n\nUse pdftk.\n",
    );
    let skill = rook_skills::Skill::load(&path, SkillSource::User).unwrap();
    assert_eq!(skill.manifest.name, "pdf");
    assert_eq!(skill.manifest.description, "Fill in PDF forms.");
    // No version declared: the bare spec has no such field, so it sorts lowest.
    assert_eq!(skill.version().to_string(), "0.0.0");
    assert!(skill.body.contains("Use pdftk."));
}

#[test]
fn preserves_unknown_frontmatter_fields() {
    let dir = tempfile::tempdir().unwrap();
    let path =
        write_skill(dir.path(), "x", "name: x\ndescription: d\nsome-other-agent-field: [a, b]\n", "body\n");
    let skill = rook_skills::Skill::load(&path, SkillSource::User).unwrap();
    assert!(
        skill.manifest.extra.contains_key("some-other-agent-field"),
        "fields from other agents must survive a round trip"
    );
}

#[test]
fn missing_frontmatter_is_a_clear_error() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().join("broken");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("SKILL.md"), "# just markdown\n").unwrap();
    assert!(matches!(rook_skills::Skill::load(&d, SkillSource::User), Err(SkillError::NoFrontmatter { .. })));
}

#[test]
fn a_typo_in_a_version_requirement_fails_at_load_time() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_skill(
        dir.path(),
        "bad",
        "name: bad\ndescription: d\nrequires:\n  language:\n    rust: \"=> 1.75\"\n",
        "body\n",
    );
    let err = rook_skills::Skill::load(&path, SkillSource::User).unwrap_err();
    assert!(
        matches!(err, SkillError::BadVersionReq { .. }),
        "a bad requirement must fail loudly, not silently never match: {err}"
    );
}

#[test]
fn requirements_gate_on_os_and_toolchain_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_skill(
        dir.path(),
        "modern-rust",
        "name: modern-rust\ndescription: d\nversion: 1.0.0\n\
         requires:\n  os: [linux, macos]\n  language:\n    rust: \">=1.85\"\n",
        "body\n",
    );
    let skill = rook_skills::Skill::load(&path, SkillSource::User).unwrap();

    assert!(skill.manifest.requires.satisfied_by(&env_linux()));

    let windows = Environment::bare("windows", "x86_64", "0.1.0").with_language("rust", "1.97.1");
    let reasons = skill.manifest.requires.check(&windows);
    assert_eq!(reasons.len(), 1, "only the OS should fail: {reasons:?}");

    let old_rust = Environment::bare("linux", "x86_64", "0.1.0").with_language("rust", "1.70.0");
    assert!(!skill.manifest.requires.satisfied_by(&old_rust));

    let no_rust = Environment::bare("linux", "x86_64", "0.1.0");
    let reasons = no_rust.languages.is_empty().then(|| skill.manifest.requires.check(&no_rust)).unwrap();
    assert!(
        reasons.iter().any(|r| r.to_string().contains("not found on PATH")),
        "a missing toolchain must be reported as missing, not as a version mismatch"
    );
}

#[test]
fn variants_select_the_most_specific_matching_body() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_skill(
        dir.path(),
        "sedish",
        "name: sedish\ndescription: In-place edits.\nversion: 1.0.0\n\
         variants:\n\
         \x20 - when: { userland: [bsd] }\n    body: variants/bsd.md\n\
         \x20 - when: { os: [windows] }\n    body: variants/windows.md\n",
        "GNU sed: `sed -i 's/a/b/' f`\n",
    );
    std::fs::create_dir_all(path.join("variants")).unwrap();
    std::fs::write(path.join("variants/bsd.md"), "BSD sed: `sed -i '' 's/a/b/' f`\n").unwrap();
    std::fs::write(path.join("variants/windows.md"), "PowerShell: use -replace\n").unwrap();

    let skill = rook_skills::Skill::load(&path, SkillSource::User).unwrap();

    let (body, variant) = skill.body_for(&env_linux()).unwrap();
    assert!(body.contains("GNU sed"), "linux should get the default body");
    assert!(variant.is_none());

    let mac = Environment::bare("macos", "aarch64", "0.1.0");
    let (body, variant) = skill.body_for(&mac).unwrap();
    assert!(body.contains("BSD sed"), "macOS has BSD userland");
    assert!(variant.is_some());

    let freebsd = Environment::bare("freebsd", "x86_64", "0.1.0");
    let (body, _) = skill.body_for(&freebsd).unwrap();
    assert!(body.contains("BSD sed"), "FreeBSD must reuse the BSD variant, not the GNU default");

    let win = Environment::bare("windows", "x86_64", "0.1.0");
    let (body, _) = skill.body_for(&win).unwrap();
    assert!(body.contains("PowerShell"));
}

#[test]
fn resolution_picks_the_newest_version_that_actually_applies() {
    let dir = tempfile::tempdir().unwrap();
    write_skill(
        dir.path(),
        "deploy/1.0.0",
        "name: deploy\ndescription: d\nversion: 1.0.0\n",
        "old and portable\n",
    );
    write_skill(
        dir.path(),
        "deploy/2.0.0",
        "name: deploy\ndescription: d\nversion: 2.0.0\nrequires:\n  tool:\n    docker: \">=27\"\n",
        "new, needs docker 27\n",
    );

    let roots = vec![(dir.path().to_path_buf(), SkillSource::User)];
    let (index, errors) = SkillIndex::discover(&roots);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(index.versions_of("deploy").len(), 2);

    // Without docker, 2.0.0 is rejected and 1.0.0 is used — with the reason kept.
    let plain = env_linux();
    let resolved = index.resolve("deploy", &plain).unwrap();
    assert_eq!(resolved.skill.version().to_string(), "1.0.0");
    assert_eq!(resolved.rejected.len(), 1);
    assert!(resolved.rejected[0].1[0].contains("docker"));

    let with_docker = env_linux().with_tool("docker", "27.1.1");
    let resolved = index.resolve("deploy", &with_docker).unwrap();
    assert_eq!(resolved.skill.version().to_string(), "2.0.0");
    assert!(resolved.rejected.is_empty());
    assert!(resolved.body.contains("needs docker 27"));
}

#[test]
fn a_project_skill_overrides_a_newer_builtin_one() {
    let builtin = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    write_skill(builtin.path(), "review", "name: review\ndescription: d\nversion: 9.9.9\n", "builtin\n");
    write_skill(project.path(), "review", "name: review\ndescription: d\nversion: 0.1.0\n", "project\n");

    let roots = vec![
        (builtin.path().to_path_buf(), SkillSource::Builtin),
        (project.path().to_path_buf(), SkillSource::Project),
    ];
    let (index, _) = SkillIndex::discover(&roots);
    let resolved = index.resolve("review", &env_linux()).unwrap();
    assert!(
        resolved.body.contains("project"),
        "a skill vendored into the project is deliberate and must win over a newer builtin"
    );
}

#[test]
fn nothing_compatible_reports_every_reason() {
    let dir = tempfile::tempdir().unwrap();
    write_skill(
        dir.path(),
        "windows-only",
        "name: windows-only\ndescription: d\nversion: 1.0.0\nrequires:\n  os: [windows]\n  tool:\n    pwsh: \">=7\"\n",
        "body\n",
    );
    let (index, _) = SkillIndex::discover(&[(dir.path().to_path_buf(), SkillSource::User)]);
    let err = index.resolve("windows-only", &env_linux()).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("os"), "{msg}");
    assert!(msg.contains("pwsh"), "{msg}");
}

#[test]
fn the_catalog_is_one_card_per_name_and_stays_small() {
    let dir = tempfile::tempdir().unwrap();
    for v in ["1.0.0", "1.2.0", "2.0.0"] {
        write_skill(
            dir.path(),
            &format!("alpha/{v}"),
            &format!("name: alpha\ndescription: Alpha skill.\nversion: {v}\n"),
            &"x".repeat(20_000),
        );
    }
    write_skill(dir.path(), "beta", "name: beta\ndescription: Beta skill.\nversion: 1.0.0\n", "short\n");

    let (index, _) = SkillIndex::discover(&[(dir.path().to_path_buf(), SkillSource::User)]);
    let cards = index.catalog(&env_linux());
    assert_eq!(cards.len(), 2, "one card per name, not per version");

    let alpha = cards.iter().find(|c| c.name == "alpha").unwrap();
    assert_eq!(alpha.version, "2.0.0");
    assert!(alpha.body_tokens > 4000, "the card should still report what loading would cost");

    // The point of the card: it is tiny compared to the body it describes.
    let card_cost: usize = cards
        .iter()
        .map(|c| rook_skills::index::estimate_tokens(&format!("{}: {}", c.name, c.description)))
        .sum();
    assert!(card_cost < 100, "catalog cost {card_cost} tokens, expected well under 100");
}

#[test]
fn inapplicable_skills_still_appear_in_the_catalog_marked_as_such() {
    let dir = tempfile::tempdir().unwrap();
    write_skill(
        dir.path(),
        "winonly",
        "name: winonly\ndescription: d\nversion: 1.0.0\nrequires:\n  os: [windows]\n",
        "body\n",
    );
    let (index, _) = SkillIndex::discover(&[(dir.path().to_path_buf(), SkillSource::User)]);
    let cards = index.catalog(&env_linux());
    assert_eq!(cards.len(), 1);
    assert!(!cards[0].applicable);
    assert!(!cards[0].mismatches.is_empty());
}

#[test]
fn version_banners_are_parsed_into_semver() {
    use rook_skills::env::extract_version;
    assert_eq!(extract_version("rustc 1.97.1 (8bab26f4f 2026-07-14)").as_deref(), Some("1.97.1"));
    assert_eq!(extract_version("git version 2.45.0").as_deref(), Some("2.45.0"));
    assert_eq!(extract_version("v20.11.1").as_deref(), Some("20.11.1"));
    assert_eq!(extract_version("Python 3.12.4").as_deref(), Some("3.12.4"));
    assert_eq!(extract_version("go version go1.22.5 darwin/arm64").as_deref(), Some("1.22.5"));
    assert_eq!(extract_version("no version here").as_deref(), None);
}

#[test]
fn one_broken_skill_does_not_take_down_the_catalog() {
    let dir = tempfile::tempdir().unwrap();
    write_skill(dir.path(), "good", "name: good\ndescription: d\nversion: 1.0.0\n", "body\n");
    let bad = dir.path().join("bad");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("SKILL.md"), "no frontmatter at all").unwrap();

    let (index, errors) = SkillIndex::discover(&[(dir.path().to_path_buf(), SkillSource::User)]);
    assert_eq!(index.len(), 1);
    assert_eq!(errors.len(), 1);
}
