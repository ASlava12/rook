//! Source-owned extension declarations. They are display data, never model
//! instructions, approval grants or evidence of current workspace/test state.

use std::io::Write;

use serde::{Deserialize, Serialize};

pub(crate) const LABEL: &str = "rook:extension-ui:v1";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub max_update_bytes: usize,
    pub max_entries: usize,
    pub max_state_bytes: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self { max_update_bytes: 4096, max_entries: 32, max_state_bytes: 32768 }
    }
}

impl Settings {
    pub(crate) fn valid(&self) -> bool {
        (1024..=32768).contains(&self.max_update_bytes)
            && (1..=128).contains(&self.max_entries)
            && (4096..=1048576).contains(&self.max_state_bytes)
    }

    fn note_bytes(&self) -> usize {
        self.max_update_bytes.saturating_add(1024).min(65536)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub event: crate::hooks::Event,
    pub ordinal: usize,
    pub digest: String,
}

impl Source {
    pub(crate) fn hook(config: &crate::hooks::HookConfig, ordinal: usize) -> Self {
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        hash.update(config.event.as_str().as_bytes());
        hash.update([0]);
        hash.update((ordinal as u64).to_le_bytes());
        hash.update(config.command.as_bytes());
        Self { event: config.event, ordinal, digest: hex::encode(hash.finalize()) }
    }

    fn valid(&self) -> bool {
        self.digest.len() == 64 && self.digest.bytes().all(|b| b.is_ascii_hexdigit())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Item {
    Status { id: String, text: String },
    Progress { id: String, label: String, done: u64, total: u64 },
    Result { id: String, title: String, body: String },
    Clear { id: String },
}

impl Item {
    fn id(&self) -> &str {
        match self {
            Self::Status { id, .. }
            | Self::Progress { id, .. }
            | Self::Result { id, .. }
            | Self::Clear { id } => id,
        }
    }

    fn valid(&self) -> bool {
        let id = self.id();
        if id.is_empty()
            || id.len() > 64
            || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return false;
        }
        let text = |value: &str, cap, multiline| {
            value.len() <= cap
                && !value.chars().any(|c| {
                    (c.is_control() && !(multiline && c == '\n'))
                        || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
                })
        };
        match self {
            Self::Status { text: value, .. } => text(value, 1024, false),
            Self::Progress { label, done, total, .. } => {
                text(label, 256, false) && *total > 0 && *total <= 9_007_199_254_740_991 && done <= total
            }
            Self::Result { title, body, .. } => text(title, 256, false) && text(body, 2048, true),
            Self::Clear { .. } => true,
        }
    }

    fn redact(&mut self, redact: &impl Fn(&str) -> String) {
        match self {
            Self::Status { id, text } => {
                *id = redact(id);
                *text = redact(text);
            }
            Self::Progress { id, label, .. } => {
                *id = redact(id);
                *label = redact(label);
            }
            Self::Result { id, title, body } => {
                *id = redact(id);
                *title = redact(title);
                *body = redact(body);
            }
            Self::Clear { id } => *id = redact(id),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Batch {
    source: Source,
    items: Vec<Item>,
}

impl Batch {
    pub(crate) fn parse(raw: &str, source: Source, settings: &Settings) -> Result<Self, &'static str> {
        if raw.len() > settings.max_update_bytes {
            return Err("extension UI update exceeds its byte limit");
        }
        struct Items(usize);
        impl<'de> serde::de::DeserializeSeed<'de> for Items {
            type Value = Vec<Item>;
            fn deserialize<D: serde::Deserializer<'de>>(
                self,
                deserializer: D,
            ) -> Result<Self::Value, D::Error> {
                deserializer.deserialize_seq(self)
            }
        }
        impl<'de> serde::de::Visitor<'de> for Items {
            type Value = Vec<Item>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a bounded UI item array")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut items = Vec::new();
                loop {
                    if items.len() == self.0 {
                        if seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
                            return Err(serde::de::Error::custom("too many UI items"));
                        }
                        break;
                    }
                    let Some(item) = seq.next_element::<Item>()? else { break };
                    items.push(item);
                }
                Ok(items)
            }
        }
        use serde::de::DeserializeSeed;
        let mut decoder = serde_json::Deserializer::from_str(raw);
        let items = Items(settings.max_entries)
            .deserialize(&mut decoder)
            .map_err(|_| "invalid extension UI declaration")?;
        decoder.end().map_err(|_| "invalid extension UI declaration")?;
        if !items.iter().all(Item::valid) {
            return Err("invalid extension UI fields");
        }
        Ok(Self { source, items })
    }

    pub(crate) fn record(
        mut self,
        rook: &crate::Rook,
        session: u128,
        redact: impl Fn(&str) -> String,
    ) -> crate::Result<()> {
        for item in &mut self.items {
            item.redact(&redact);
        }
        if !self.items.iter().all(Item::valid) {
            return Ok(());
        }
        let body = encoded(&self, rook.config.extension_ui.note_bytes()).map_err(|_| {
            crate::error::CoreError::Other("extension UI record exceeds its byte limit".into())
        })?;
        let body = String::from_utf8(body)
            .map_err(|_| crate::error::CoreError::Other("extension UI encoding was not UTF-8".into()))?;
        rook.log(session, rook_store::EventKind::Note, LABEL, &body)?;
        Ok(())
    }
}

struct Limited {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for Limited {
    fn write(&mut self, chunk: &[u8]) -> std::io::Result<usize> {
        if chunk.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("encoded UI limit"));
        }
        self.bytes.extend_from_slice(chunk);
        Ok(chunk.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(crate) fn encoded(value: &impl Serialize, limit: usize) -> Result<Vec<u8>, serde_json::Error> {
    let mut writer = Limited { bytes: Vec::new(), limit };
    serde_json::to_writer(&mut writer, value)?;
    Ok(writer.bytes)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub source: Source,
    pub event_seq: u64,
    pub item: Item,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub reports: Vec<Report>,
    pub omitted_updates: usize,
    pub invalid_records: usize,
    #[serde(skip)]
    bytes: usize,
}

impl State {
    pub(crate) fn include(
        &mut self,
        store: &rook_store::Store,
        event: &rook_store::Event,
        bytes: u64,
        settings: &Settings,
    ) -> crate::Result<()> {
        if event.record.kind != rook_store::EventKind::Note || event.record.label != LABEL {
            return Ok(());
        }
        if bytes > settings.note_bytes() as u64 {
            self.invalid_records = self.invalid_records.saturating_add(1);
            return Ok(());
        }
        let raw = store.get_range(&event.record.body, 0, bytes as usize)?;
        let batch = serde_json::from_slice::<Batch>(&raw).ok().filter(|b| {
            b.source.valid() && b.items.len() <= settings.max_entries && b.items.iter().all(Item::valid)
        });
        let Some(batch) = batch else {
            self.invalid_records = self.invalid_records.saturating_add(1);
            return Ok(());
        };
        for item in batch.items {
            let previous =
                self.reports.iter().position(|r| r.source == batch.source && r.item.id() == item.id());
            let old = previous
                .and_then(|at| encoded(&self.reports[at], settings.max_state_bytes).ok())
                .map_or(0, |v| v.len());
            if matches!(item, Item::Clear { .. }) {
                if let Some(at) = previous {
                    self.reports.remove(at);
                    self.bytes = self.bytes.saturating_sub(old);
                }
                continue;
            }
            let report = Report { source: batch.source.clone(), event_seq: event.seq, item };
            let size = encoded(&report, settings.max_state_bytes).ok().map(|v| v.len());
            let Some(size) = size.filter(|size| {
                self.bytes.saturating_sub(old).saturating_add(*size) <= settings.max_state_bytes
                    && (previous.is_some() || self.reports.len() < settings.max_entries)
            }) else {
                self.omitted_updates = self.omitted_updates.saturating_add(1);
                continue;
            };
            self.bytes = self.bytes.saturating_sub(old).saturating_add(size);
            match previous {
                Some(at) => self.reports[at] = report,
                None => self.reports.push(report),
            }
        }
        Ok(())
    }

    pub fn describe(&self) -> String {
        if self.reports.is_empty() && self.omitted_updates == 0 && self.invalid_records == 0 {
            return String::new();
        }
        let mut lines = vec![
            "Extension reports · saved branch history".to_string(),
            "Reported by extensions; current files and tests are not verified.".into(),
        ];
        for report in &self.reports {
            lines.push(format!(
                "hook {} #{} · source {} · event #{}",
                report.source.event.as_str(),
                report.source.ordinal.saturating_add(1),
                report.source.digest.get(..8).unwrap_or("unknown"),
                report.event_seq
            ));
            lines.push(match &report.item {
                Item::Status { id, text } => format!("{id}: {text}"),
                Item::Progress { id, label, done, total } => format!("{id}: {label} · {done}/{total}"),
                Item::Result { id, title, body } => format!("{id}: {title}\n{body}"),
                Item::Clear { .. } => String::new(),
            });
        }
        if self.omitted_updates > 0 || self.invalid_records > 0 {
            lines.push(format!(
                "{} omitted updates · {} invalid records; displayed reports may be older.",
                self.omitted_updates, self.invalid_records
            ));
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(ordinal: usize) -> Source {
        Source::hook(
            &crate::hooks::HookConfig { command: "echo report".into(), ..Default::default() },
            ordinal,
        )
    }

    #[test]
    fn declarations_refuse_bytes_counts_invalid_fields_and_forged_sources() {
        let settings = Settings { max_entries: 1, ..Default::default() };
        let valid = r#"[{"kind":"status","id":"build","text":"checking"}]"#;
        assert!(Batch::parse(valid, source(0), &settings).is_ok());
        for raw in [
            format!("[{0},{0}]", &valid[1..valid.len() - 1]),
            r#"[{"kind":"status","id":"x","text":"x","source":"trusted"}]"#.into(),
            r#"[{"kind":"progress","id":"x","label":"build","done":2,"total":1}]"#.into(),
            r#"[{"kind":"progress","id":"x","label":"build","done":1,"total":9007199254740992}]"#.into(),
            r#"[{"kind":"status","id":"x","text":"escape\u001b[2J"}]"#.into(),
            r#"[{"kind":"status","id":"x","text":"direction\u202e"}]"#.into(),
            " ".repeat(settings.max_update_bytes + 1),
        ] {
            assert!(Batch::parse(&raw, source(0), &settings).is_err(), "{raw}");
        }
        assert_ne!(source(0), source(1));
        let changed =
            Source::hook(&crate::hooks::HookConfig { command: "echo other".into(), ..Default::default() }, 0);
        assert_ne!(source(0).digest, changed.digest);
    }

    #[test]
    fn encoding_limits_apply_before_the_write_and_serialization_completes() {
        let value = serde_json::json!({"text": "x".repeat(2048)});
        assert!(encoded(&value, 1024).is_err());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&encoded(&value, 4096).unwrap()).unwrap(),
            value
        );
        let mut writer = Limited { bytes: vec![1, 2], limit: 3 };
        assert!(writer.write_all(&[3, 4]).is_err());
        assert_eq!(writer.bytes, [1, 2]);
    }

    #[test]
    fn old_and_untrusted_api_state_has_safe_text_fallback() {
        let old: State = serde_json::from_str("{}").unwrap();
        assert!(old.describe().is_empty());
        let state = State {
            reports: vec![Report {
                source: Source { digest: "x".into(), ..source(0) },
                event_seq: 5,
                item: Item::Status { id: "build".into(), text: "reported success".into() },
            }],
            ..Default::default()
        };
        assert!(state.describe().contains("source unknown"));
        assert!(state.describe().contains("current files and tests are not verified"));
    }

    #[test]
    fn display_records_redact_before_storage_and_revalidate_expanded_fields() {
        let dir = tempfile::tempdir().unwrap();
        let rook = crate::Rook::from_parts(
            rook_store::Store::open(dir.path().join("store")).unwrap(),
            crate::Config::default(),
            rook_skills::Environment::bare("test", "test", "0.10.0"),
            rook_skills::SkillIndex::default(),
            dir.path().to_path_buf(),
        );
        let session = rook.start_session("redaction").unwrap();
        let batch = Batch::parse(
            r#"[{"kind":"result","id":"report","title":"secret","body":"secret output"}]"#,
            source(0),
            &Settings::default(),
        )
        .unwrap();
        batch.record(&rook, session, |s| s.replace("secret", "[redacted]")).unwrap();
        let event = rook.store.events(session, 0, 1).unwrap().pop().unwrap();
        let body = String::from_utf8(rook.store.get(&event.record.body).unwrap()).unwrap();
        assert!(!body.contains("secret"));
        assert!(body.contains("[redacted] output"));
        let batch = Batch::parse(
            r#"[{"kind":"status","id":"report","text":"secret"}]"#,
            source(0),
            &Settings::default(),
        )
        .unwrap();
        batch.record(&rook, session, |_| "x".repeat(4096)).unwrap();
        assert_eq!(
            rook.store.events(session, 0, 10).unwrap().len(),
            1,
            "expanded invalid redaction is never logged"
        );
    }
}
