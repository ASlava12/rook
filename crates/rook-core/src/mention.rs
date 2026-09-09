//! Naming a file in a prompt without typing its path.
//!
//! Referring to a file meant knowing where it was and spelling it out, so the
//! shortest way to ask about one was to leave the window, find it, and paste
//! it back. What a person knows is the name; the path is what the agent needs.

use std::path::Path;

/// Every file in the workspace, bounded, as paths relative to it.
///
/// Walked once when a mention starts rather than once per keystroke: the cap
/// is twenty thousand files, and a front end redrawing every sixty
/// milliseconds cannot pay that to narrow a list it already has.
pub fn here(workspace: &Path, most_files: usize) -> Vec<String> {
    let mut found = Vec::new();
    // `require_git` is not the default: without it a `.gitignore` is ignored
    // outside a repository, and a workspace need not be one. Not following
    // links is the default, and stated because it is the workspace boundary.
    for entry in ignore::WalkBuilder::new(workspace).follow_links(false).require_git(false).build().flatten()
    {
        // A walk with no end is a hang, and it cannot tell a workspace from a
        // home directory until it is inside one. The same cap the search tool
        // puts on the looking rather than a second knob meaning the same thing.
        if found.len() >= most_files {
            break;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if let Ok(path) = entry.path().strip_prefix(workspace) {
            found.push(path.display().to_string());
        }
    }
    found
}

/// Which of them a fragment names, best first.
///
/// Ranked by where the fragment matched, because that is what a person meant:
/// a name that starts with what was typed is what they were typing, a name
/// that merely contains it is a maybe, and a match somewhere up the directory
/// path is the last resort. Ties go to the shorter path — the one nearer the
/// root is the one more likely meant, and a deeply nested near-duplicate is
/// what pushes the wanted answer off a short list.
///
/// An empty fragment is a menu of what is here rather than nothing: `@` alone
/// is a question, and answering it with a blank pane teaches nobody the
/// gesture.
pub fn matching(paths: &[String], fragment: &str, limit: usize) -> Vec<String> {
    let wanted = fragment.to_lowercase();
    let mut found: Vec<(u8, usize, &String)> = Vec::new();
    for shown in paths {
        let low = shown.to_lowercase();
        let name = low.rsplit(['/', '\\']).next().unwrap_or(&low);
        let rank = match () {
            _ if wanted.is_empty() => 0,
            _ if name.starts_with(&wanted) => 0,
            _ if name.contains(&wanted) => 1,
            _ if low.contains(&wanted) => 2,
            _ => continue,
        };
        found.push((rank, shown.chars().count(), shown));
    }
    found.sort();
    found.into_iter().take(limit).map(|(.., shown)| shown.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for path in files {
            let at = dir.path().join(path);
            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
            std::fs::write(at, "x").unwrap();
        }
        dir
    }

    #[test]
    fn where_the_fragment_matched_is_what_orders_the_answers() {
        let dir = workspace(&[
            "service/notes.md",       // only the directory matches
            "src/my_service.rs",      // the name contains it
            "service.toml",           // the name starts with it
            "deep/deep/service.toml", // as does this, further away
        ]);
        let found = matching(&here(dir.path(), 1000), "service", 10);
        // Asked for rather than spelled: these are paths, and the separator is
        // the platform's. `deep/deep/service.toml` passed here and failed on
        // the Windows runner, which is the third time that has happened.
        let spelt = |path: &str| path.split('/').collect::<std::path::PathBuf>().display().to_string();
        assert_eq!(
            found,
            ["service.toml", "deep/deep/service.toml", "src/my_service.rs", "service/notes.md"]
                .map(spelt)
                .to_vec(),
            "starts-with before contains before a match up the path, and the nearer of two equals first"
        );
    }

    #[test]
    fn a_gesture_with_nothing_typed_after_it_still_answers() {
        let dir = workspace(&["a.rs", "b.rs"]);
        let all = here(dir.path(), 1000);
        assert_eq!(matching(&all, "", 10).len(), 2, "`@` alone is a question");
        assert!(matching(&all, "nothing-like-this", 10).is_empty(), "and a miss is a miss");
    }

    /// The walk cannot tell a workspace from a home directory until it is in
    /// one, so what it may look at is capped — and the cap has to be reached
    /// for this to be testing it.
    #[test]
    fn the_looking_is_bounded_and_not_only_the_list() {
        let many: Vec<String> = (0..60).map(|n| format!("file{n:02}.rs")).collect();
        let dir = workspace(&many.iter().map(String::as_str).collect::<Vec<_>>());
        let found = matching(&here(dir.path(), 20), "file", 100);
        assert_eq!(found.len(), 20, "it stopped looking at twenty of sixty: {}", found.len());
    }
}
