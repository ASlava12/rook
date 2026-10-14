use super::*;
use crate::memory::{Fact, MemoryBook, Scope};
use rook_contain::files::RecoveryFiles;
use rook_store::{Kind, SessionMeta, SessionRefUpdate};
use std::path::{Path, PathBuf};
const MEMORY_HEAD: &str = "memory/head";
struct Plan {
    effect: Effect,
    head: Option<ObjectId>,
    memory: Option<MemoryBook>,
    current: Option<Snapshot>,
    token: String,
    diff: Value,
}
pub(super) fn validate_effect(p: &Proposal, s: &Settings) -> Result<()> {
    for effect in p.effect.iter().chain(p.original_effect.iter()) {
        match effect {
            Effect::Memory { before, after } => {
                for fact in before.iter().chain(std::iter::once(after)) {
                    if fact.text.len() > 8192 || Fact::new(&fact.text, fact.scope.clone()).id != fact.id {
                        return Err(bad("invalid retained learning memory effect"));
                    }
                }
            }
            Effect::Skill { name, directory, before, after, changed, versions } => {
                checked_directory(directory)?;
                if !rook_skills::usable_name(name)
                    || changed.len() > 2 * s.skill_files
                    || changed.iter().any(|p| !relative(p))
                    || versions.len() > 4
                    || versions.iter().any(|v| ObjectId::from_hex(v).is_none())
                {
                    return Err(bad("invalid skill effect metadata"));
                }
                for snap in [before, after] {
                    if snap.files.len() > s.skill_files || snap.source != hash(&snap.files, s.state_bytes)? {
                        return Err(bad("learning skill snapshot count or identity mismatch"));
                    }
                    let mut total = 0usize;
                    for (path, f) in &snap.files {
                        total = total.saturating_add(f.bytes);
                        if !relative(path)
                            || ObjectId::from_hex(&f.object).is_none()
                            || f.bytes > s.skill_file_bytes
                            || f.mode > 0o777
                            || total > s.skill_total_bytes
                        {
                            return Err(bad("learning skill source exceeds its byte/mode budget"));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
fn memory(rook: &Rook) -> Result<(Option<ObjectId>, MemoryBook)> {
    let head = rook.store.get_ref(MEMORY_HEAD)?;
    let book = match head {
        Some(id) => {
            let max = rook.config.learning.memory_bytes;
            let size =
                rook.store.stat_object(&id)?.ok_or_else(|| bad("memory head object missing"))?.size_raw;
            if size > max as u64 {
                return Err(bad(
                    "memory exceeds learning.memory_bytes; no prefix can be treated as the current book",
                ));
            }
            let bytes = rook.store.get_range(&id, 0, max + 1)?;
            if ObjectId::of(&bytes) != id {
                return Err(bad("memory head checksum mismatch"));
            }
            serde_json::from_slice(&bytes)?
        }
        None => MemoryBook::default(),
    };
    Ok((head, book))
}
fn skill_root(rook: &Rook, name: &str) -> Result<PathBuf> {
    if !rook_skills::usable_name(name) {
        return Err(bad("invalid retained skill name"));
    }
    if let Ok(existing) = rook.skills().resolve(name, rook.env()) {
        if existing.skill.source != rook_skills::SkillSource::User {
            return Err(bad("system/project/plugin skills cannot be replaced by a learning proposal"));
        }
        checked_directory(&existing.skill.dir.display().to_string())?;
        return Ok(existing.skill.dir.clone());
    }
    Ok(crate::paths::user_skills_dir().join(name))
}
fn checked_directory(directory: &str) -> Result<PathBuf> {
    let path = PathBuf::from(directory);
    let root = crate::paths::user_skills_dir();
    let inside = path
        .strip_prefix(&root)
        .map_err(|_| bad("retained learning skill directory is outside the current user skill root"))?;
    if inside.components().count() != 1 || !relative(&inside.to_string_lossy()) {
        return Err(bad("learning skill directory must be an owned direct child"));
    }
    Ok(path)
}
fn skill_source(rook: &Rook, directory: &Path) -> Result<Snapshot> {
    checked_directory(&directory.display().to_string())?;
    if !directory.exists() {
        return Ok(Snapshot {
            source: hash(&BTreeMap::<String, File>::new(), rook.config.learning.state_bytes)?,
            files: BTreeMap::new(),
        });
    }
    crate::shell_undo::capture(&rook.store, directory, &rook.config.learning.capture(), true)
}
fn data(rook: &Rook, f: &File) -> Result<Vec<u8>> {
    let id = ObjectId::from_hex(&f.object).ok_or_else(|| bad("invalid skill source object"))?;
    let bytes = rook.store.get_range(&id, 0, f.bytes + 1)?;
    if bytes.len() != f.bytes || ObjectId::of(&bytes) != id {
        return Err(bad("skill source missing or corrupt"));
    }
    Ok(bytes)
}
fn skill_target(
    rook: &Rook,
    skill: &crate::AuthoredSkill,
    remove: &[String],
    before: &Snapshot,
) -> Result<Snapshot> {
    if let Ok(existing) = rook.skills().resolve(&skill.name, rook.env())
        && existing.skill.source != rook_skills::SkillSource::User
    {
        return Err(bad("system/project/plugin skills cannot be replaced by a learning proposal"));
    }
    let mut after = before.clone();
    for path in remove {
        after.files.remove(path);
    }
    let (mut manifest, body) = rook_skills::parse_manifest(&skill.to_skill_md()?, Path::new("SKILL.md"))?;
    if let Some(old) = before.files.get("SKILL.md") {
        let bytes = data(rook, old)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| bad("existing skill card is not UTF-8"))?;
        let (old_manifest, _) = rook_skills::parse_manifest(text, Path::new("SKILL.md"))?;
        manifest.version = old_manifest.version;
    }
    let card = manifest.to_skill_md(&body)?;
    for (path, text) in std::iter::once((&"SKILL.md".to_owned(), &card)).chain(skill.files.iter()) {
        let mode = rook_contain::files::restored_file_mode(
            before.files.get(path).map_or(if text.starts_with("#!") { 0o755 } else { 0o600 }, |f| f.mode),
        );
        after.files.insert(
            path.clone(),
            File {
                object: rook.store.put(Kind::FileBlob, text.as_bytes())?.to_hex(),
                bytes: text.len(),
                mode,
            },
        );
    }
    after.source = hash(&after.files, rook.config.learning.state_bytes)?;
    Ok(after)
}
fn plan(rook: &Rook, owner: u128, p: &Proposal, rollback: bool) -> Result<Plan> {
    let s = &rook.config.learning;
    if let Some(session) = p.session.as_deref().and_then(rook_store::parse_session_id) {
        if crate::execution::is_active(rook, session) {
            return Err(bad("the learning change is still live"));
        }
        if let Some(reason) = rook.recovery_block(session)? {
            return Err(bad(reason));
        }
    }
    if (!rollback && p.state != "proposed")
        || (rollback && !matches!(p.state.as_str(), "applied" | "partial" | "rollback_partial"))
    {
        return Err(bad("inspect this proposal's terminal or recovery state before preparing a change"));
    }
    let (effect, head, mem, current, diff) = match (&p.draft.target, rollback) {
        (Target::Memory { text, supersedes }, false) => {
            let (head, mut book) = memory(rook)?;
            let before = supersedes
                .as_ref()
                .map(|id| {
                    book.get(id)
                        .cloned()
                        .ok_or_else(|| bad("the fact selected for correction no longer exists"))
                })
                .transpose()?;
            if before.as_ref().is_some_and(|f| !f.scope.applies_in(&rook.workspace.display().to_string())) {
                return Err(bad("a proposal cannot correct another workspace's memory"));
            }
            if book.facts.iter().any(|f| f.text.trim() == text.trim() && supersedes.as_ref() != Some(&f.id)) {
                return Err(bad("this fact is already remembered; do not apply a duplicate proposal"));
            }
            let seq = p.draft.evidence[0];
            let mut after = Fact::new(
                text,
                before
                    .as_ref()
                    .map(|f| f.scope.clone())
                    .unwrap_or_else(|| Scope::Project(rook.workspace.display().to_string())),
            )
            .from_turn(owner, seq);
            if let Some(old) = &before {
                if old.text.trim() == text.trim() {
                    return Err(bad("this correction does not change the remembered text"));
                }
                after.tags = old.tags.clone();
                after.pinned = old.pinned;
            }
            if let Some(f) = &before {
                book.forget(&f.id);
            }
            if book.get(&after.id).is_some() {
                return Err(bad("memory identity collision or duplicate; inspect the current fact"));
            }
            book.learn(after.clone());
            let diff = json!({"kind":"memory","before":before,"after":after});
            (Effect::Memory { before, after }, head, Some(book), None, diff)
        }
        (Target::Memory { .. }, true) => {
            let Some(Effect::Memory { before, after }) = &p.effect else {
                return Err(bad("retained memory inverse missing"));
            };
            let (head, mut book) = memory(rook)?;
            if book.get(&after.id) != Some(after) {
                return Err(bad("the applied fact changed; rollback must not remove a later correction"));
            }
            book.forget(&after.id);
            if let Some(old) = before {
                if book.get(&old.id).is_some() {
                    return Err(bad(
                        "the previous fact already has a newer replacement; inspect memory before rollback",
                    ));
                }
                book.learn(old.clone());
            }
            (
                p.effect.clone().ok_or_else(|| bad("memory inverse missing"))?,
                head,
                Some(book),
                None,
                json!({"kind":"memory","before":after,"after":before}),
            )
        }
        (Target::Skill { skill, remove }, false) => {
            let directory = skill_root(rook, &skill.name)?;
            let before = skill_source(rook, &directory)?;
            let after = skill_target(rook, skill, remove, &before)?;
            let changed: Vec<_> = before
                .files
                .keys()
                .chain(after.files.keys())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .filter(|path| before.files.get(*path) != after.files.get(*path))
                .cloned()
                .collect();
            if changed.is_empty() {
                return Err(bad("the proposed skill is already present; no change to apply"));
            }
            let effect = Effect::Skill {
                name: skill.name.clone(),
                directory: directory.display().to_string(),
                before: before.clone(),
                after: after.clone(),
                changed: changed.clone(),
                versions: Vec::new(),
            };
            let diff = skill_diff(rook, &before, &after, &changed)?;
            (effect, None, None, Some(before), diff)
        }
        (Target::Skill { .. }, true) => {
            let Some(Effect::Skill { name, directory, before, after, changed, versions }) =
                &p.original_effect
            else {
                return Err(bad("retained skill inverse missing"));
            };
            let current = skill_source(rook, &checked_directory(directory)?)?;
            let selected = if p.state == "partial" { p.completed.clone() } else { changed.clone() };
            let mut target = current.clone();
            let mut remaining = Vec::new();
            for path in &selected {
                if current.files.get(path) == before.files.get(path) {
                    continue;
                }
                if current.files.get(path) != after.files.get(path) {
                    return Err(bad(format!(
                        "skill path {path} changed after application; preserve that edit before rollback"
                    )));
                }
                remaining.push(path.clone());
                match before.files.get(path) {
                    Some(f) => {
                        target.files.insert(path.clone(), f.clone());
                    }
                    None => {
                        target.files.remove(path);
                    }
                }
            }
            if remaining.is_empty() {
                return Err(bad("this partial application has no observed completed files to roll back"));
            }
            target.source = hash(&target.files, s.state_bytes)?;
            let diff = skill_diff(rook, &current, &target, &remaining)?;
            (
                Effect::Skill {
                    name: name.clone(),
                    directory: directory.clone(),
                    before: current.clone(),
                    after: target,
                    changed: remaining,
                    versions: versions.clone(),
                },
                None,
                None,
                Some(current),
                diff,
            )
        }
    };
    let holder = Proposal { effect: Some(effect.clone()), ..p.clone() };
    validate_effect(&holder, s)?;
    let token = hash(
        &(owner, &p.id, &p.source.signature, &p.draft, rollback, &head, &current, &effect),
        s.state_bytes,
    )?;
    if let Some(book) = &mem {
        crate::persistence::encode_with_limit(book, s.memory_bytes)?;
    }
    Ok(Plan { effect, head, memory: mem, current, token, diff })
}
fn skill_diff(rook: &Rook, before: &Snapshot, after: &Snapshot, paths: &[String]) -> Result<Value> {
    let mut entries = Vec::new();
    let mut budget = rook.config.learning.output_bytes;
    for path in paths {
        let old = before.files.get(path).map(|f| data(rook, f)).transpose()?.unwrap_or_default();
        let new = after.files.get(path).map(|f| data(rook, f)).transpose()?.unwrap_or_default();
        let old = String::from_utf8_lossy(&old);
        let new = String::from_utf8_lossy(&new);
        let mut diff = String::new();
        for change in similar::TextDiff::configure()
            .deadline(std::time::Instant::now() + std::time::Duration::from_secs(1))
            .diff_lines(old.as_ref(), new.as_ref())
            .iter_all_changes()
        {
            let marker = match change.tag() {
                similar::ChangeTag::Delete => '-',
                similar::ChangeTag::Insert => '+',
                similar::ChangeTag::Equal => ' ',
            };
            let line = format!("{marker}{change}");
            if diff.len().saturating_add(line.len()) > budget {
                break;
            }
            diff.push_str(&line);
        }
        budget = budget.saturating_sub(diff.len());
        entries.push(json!({"path":path,"before":before.files.get(path),"after":after.files.get(path),"diff":diff,"omitted":old.len().saturating_add(new.len())>diff.len()}));
    }
    Ok(json!({"kind":"skill","files":entries}))
}
pub(super) fn prepare(rook: &Rook, owner: u128, book: &Book, id: &str, rollback: bool) -> Result<Value> {
    let p = &book.proposals[position(book, id)?];
    let plan = plan(rook, owner, p, rollback)?;
    Ok(
        json!({"proposal":id,"token":plan.token,"rollback":rollback,"diff":plan.diff,"source":p.source,"draft":p.draft}),
    )
}
pub(super) fn apply(
    rook: &Rook,
    owner: u128,
    book: &mut Book,
    id: &str,
    expected: &str,
    rollback: bool,
) -> Result<Value> {
    let index = position(book, id)?;
    let prepared = plan(rook, owner, &book.proposals[index], rollback)?;
    if prepared.token != expected {
        return Err(bad("proposal or target changed; review a fresh prepare token"));
    }
    let old_receipt = rook.store.kv_get_limited(&key(owner), rook.config.learning.state_bytes)?;
    if let Some(mut memory) = prepared.memory {
        memory.updated_at = rook_store::now_unix();
        memory.note = Some(format!(
            "{} learning proposal {id} from {}:{}",
            if rollback { "rolled back" } else { "applied" },
            rook_store::format_session_id(owner),
            book.proposals[index].source.result
        ));
        let bytes = crate::persistence::encode_with_limit(&memory, rook.config.learning.memory_bytes)?;
        let object = rook.store.put(Kind::Memory, &bytes)?;
        let p = &mut book.proposals[index];
        p.state = if rollback { "rolled_back" } else { "applied" }.into();
        p.effect = Some(prepared.effect);
        p.error = None;
        let encoded = encode(rook, book)?;
        let history = format!("memory/h/{}", rook_store::history_key());
        if !rook.store.compare_set_ref_session(SessionRefUpdate {
            session: owner,
            reference: MEMORY_HEAD,
            expected: prepared.head,
            object,
            history: &history,
            companion: &key(owner),
            expected_companion: old_receipt.as_deref(),
            value: &encoded,
            maximum: rook.config.learning.state_bytes,
        })? {
            return Err(bad(
                "memory or proposal moved during application; no head/receipt was changed; review again",
            ));
        }
        return Ok(show(&book.proposals[index]));
    }
    apply_skill(rook, owner, book, index, prepared, rollback)
}
fn version(rook: &Rook, name: &str, directory: &str, snapshot: &Snapshot) -> Result<String> {
    let declared = snapshot
        .files
        .get("SKILL.md")
        .map(|f| -> Result<String> {
            let bytes = data(rook, f)?;
            let text = std::str::from_utf8(&bytes).map_err(|_| bad("retained skill card is not UTF-8"))?;
            let (manifest, _) = rook_skills::parse_manifest(text, Path::new("SKILL.md"))?;
            Ok(manifest.version)
        })
        .transpose()?
        .unwrap_or_default();
    let set = crate::FileSet {
        kind: "skill".into(),
        name: name.into(),
        version: declared,
        root: checked_directory(directory)?.display().to_string(),
        files: snapshot.files.iter().map(|(p, f)| (p.clone(), f.object.clone())).collect(),
        absent: Vec::new(),
        total_bytes: snapshot.files.values().map(|f| f.bytes as u64).sum(),
        captured_at: rook_store::now_unix(),
        note: Some(
            "Reviewed learning proposal source; restoration must use its conflict-checked receipt.".into(),
        ),
    };
    let bytes = crate::persistence::encode_with_limit(&set, rook.config.learning.state_bytes)?;
    let object = rook.store.put(Kind::Snapshot, &bytes)?;
    rook.store.set_ref(&format!("skill/{name}/h/{}", rook_store::history_key()), &object)?;
    Ok(object.to_hex())
}
fn reserve(rook: &Rook, book: &Book) -> Result<()> {
    let mut projection = book.clone();
    for p in &mut projection.proposals {
        if matches!(p.state.as_str(), "applying" | "rolling_back") {
            p.error = Some("\0".repeat(4096));
            p.note = Some("\0".repeat(4096));
            if let Some(Effect::Skill { changed, versions, .. }) = &mut p.effect {
                p.completed = changed.clone();
                p.pending = changed.iter().max_by_key(|p| p.len()).cloned();
                while versions.len() < 4 {
                    versions.push("f".repeat(64));
                }
            }
        }
    }
    encode(rook, &projection)?;
    Ok(())
}
fn apply_skill(
    rook: &Rook,
    owner: u128,
    book: &mut Book,
    index: usize,
    plan: Plan,
    rollback: bool,
) -> Result<Value> {
    let Effect::Skill { name, directory, before, after, changed, mut versions } = plan.effect else {
        return Err(bad("skill change plan missing"));
    };
    let current = plan.current.ok_or_else(|| bad("skill preimage missing"))?;
    if current.source != before.source {
        return Err(bad("skill preimage plan mismatch"));
    }
    for f in before.files.values().chain(after.files.values()) {
        data(rook, f)?;
    }
    // A preimage version is retained before the first possible filesystem write.
    if book.proposals[index].operations >= 8 {
        return Err(bad(
            "eight learning filesystem operations reached; retain the receipts and repair manually",
        ));
    }
    let prior = version(rook, &name, &directory, &before)?;
    if versions.len() < 4 {
        versions.push(prior);
    }
    let session = rook_store::new_session_id();
    let root = checked_directory(&directory)?;
    let mut meta = SessionMeta::new(
        session,
        format!("Learning change {}", book.proposals[index].id),
        root.display().to_string(),
        rook_store::now_unix(),
    );
    meta.parent = Some(owner);
    meta.tags = vec!["learning-apply".into()];
    let p = &mut book.proposals[index];
    if !rollback {
        p.original_effect = Some(Effect::Skill {
            name: name.clone(),
            directory: directory.clone(),
            before: before.clone(),
            after: after.clone(),
            changed: changed.clone(),
            versions: versions.clone(),
        });
    }
    p.operations += 1;
    p.effect = Some(Effect::Skill {
        name: name.clone(),
        directory: directory.clone(),
        before: before.clone(),
        after: after.clone(),
        changed: changed.clone(),
        versions,
    });
    p.state = if rollback { "rolling_back" } else { "applying" }.into();
    p.session = Some(rook_store::format_session_id(session));
    p.pending = None;
    p.completed.clear();
    p.error = None;
    reserve(rook, book)?;
    rook.store.create_session(&meta)?;
    save(rook, owner, book)?;
    let engine = rook.for_workspace(root.clone());
    let journal = crate::execution::Journal::start(&engine, session, None, false)?;
    journal.admit("Apply the explicitly reviewed learning change")?;
    journal.begin(
        "apply_learning",
        &json!({"proposal":book.proposals[index].id,"rollback":rollback}).to_string(),
        true,
        false,
        None,
    )?;
    let action = (|| -> Result<()> {
        RecoveryFiles::open(&crate::paths::home(), Path::new("skills"), true)
            .map_err(|e| bad(e.to_string()))?;
        let files = RecoveryFiles::open(
            &crate::paths::user_skills_dir(),
            root.strip_prefix(crate::paths::user_skills_dir())
                .map_err(|_| bad("skill directory escaped user root"))?,
            true,
        )
        .map_err(|e| bad(e.to_string()))?;
        if skill_source(rook, &root)?.source != before.source {
            return Err(bad("skill changed after review; no selected path will be overwritten"));
        }
        for path in &changed {
            book.proposals[index].pending = Some(path.clone());
            save(rook, owner, book)?;
            let old = before.files.get(path);
            let new = after.files.get(path);
            let old_data = old.map(|f| data(rook, f)).transpose()?;
            let new_data = new.map(|f| data(rook, f)).transpose()?;
            if !files
                .replace_checked(
                    Path::new(path),
                    old_data.as_deref(),
                    old.map(|f| f.mode),
                    new_data.as_deref(),
                    new.map_or(0o600, |f| f.mode),
                )
                .map_err(|e| bad(e.to_string()))?
            {
                return Err(bad(format!("skill path {path} changed; preserve it and inspect recovery")));
            }
            book.proposals[index].completed.push(path.clone());
            book.proposals[index].pending = None;
            save(rook, owner, book)?;
        }
        Ok(())
    })();
    if let Err(error) = action {
        book.proposals[index].state = if rollback { "rolling_back" } else { "partial" }.into();
        book.proposals[index].error = Some(rook_tools::elide_middle(&error.to_string(), 4096));
        save(rook, owner, book)?;
        // The pending operation stays unknown until explicitly inspected.
        return Err(error);
    }
    let observed = skill_source(rook, &root)?;
    if observed.source != after.source {
        return Err(bad("skill changed during publication; inspect its saved applying receipt"));
    }
    let next = version(rook, &name, &directory, &observed)?;
    if let Some(Effect::Skill { versions, .. }) = &mut book.proposals[index].effect
        && versions.len() < 4
    {
        versions.push(next);
    }
    book.proposals[index].state = if rollback { "rolled_back" } else { "applied" }.into();
    save(rook, owner, book)?;
    journal.complete("Reviewed skill files published with matching source identities", None, None)?;
    journal.finish("complete", None)?;
    rook.reload_skills();
    Ok(show(&book.proposals[index]))
}
pub(super) fn recover(rook: &Rook, owner: u128, book: &mut Book, id: &str, note: &str) -> Result<Value> {
    if note.trim().is_empty() || note.len() > 4096 {
        return Err(bad("record a recovery inspection note of 1..4096 bytes"));
    }
    let index = position(book, id)?;
    let p = &book.proposals[index];
    if !matches!(p.state.as_str(), "applying" | "rolling_back" | "partial") {
        return Err(bad("this proposal has no uncertain filesystem change"));
    }
    let session = p
        .session
        .as_deref()
        .and_then(rook_store::parse_session_id)
        .ok_or_else(|| bad("change recovery session missing"))?;
    if crate::execution::is_active(rook, session) {
        return Err(bad("learning change worker is still active"));
    }
    if let Some(reason) = rook.recovery_block(session)? {
        return Err(bad(reason));
    }
    let Some(Effect::Skill { directory, before, after, changed, .. }) = &p.effect else {
        return Err(bad("skill recovery source missing"));
    };
    let current = skill_source(rook, &checked_directory(directory)?)?;
    let mut completed = Vec::new();
    for path in changed {
        if current.files.get(path) == after.files.get(path) {
            completed.push(path.clone());
        } else if current.files.get(path) != before.files.get(path) {
            return Err(bad(format!(
                "skill path {path} matches neither saved source; preserve the external edit before recovery"
            )));
        }
    }
    let full = completed.len() == changed.len();
    let rollback = p.state == "rolling_back";
    let p = &mut book.proposals[index];
    p.completed = completed;
    p.pending = None;
    p.note = Some(note.into());
    p.state = if full {
        if rollback { "rolled_back" } else { "applied" }
    } else {
        if rollback { "rollback_partial" } else { "partial" }
    }
    .into();
    save(rook, owner, book)?;
    rook.reload_skills();
    Ok(show(&book.proposals[index]))
}
