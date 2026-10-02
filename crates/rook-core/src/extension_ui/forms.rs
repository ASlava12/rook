//! Bounded typed forms use the existing question channel. Answers go to their
//! extension without automatic model/journal recording; saved reports keep only
//! status. A producer can still explicitly echo values in its reply context.
use rook_tools::ask::{Answer, Asker, Question};
use serde::{Deserialize, Serialize};

use super::{Batch, Item, Settings, Source};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Form {
    pub id: String,
    title: String,
    #[serde(deserialize_with = "four")]
    fields: Vec<Field>,
}

fn four<'de, D, T>(decoder: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Four<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Four<T> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("one to four entries")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut entries = Vec::new();
            loop {
                if entries.len() == 4 {
                    if seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
                        return Err(serde::de::Error::custom("too many form entries"));
                    }
                    break;
                }
                let Some(entry) = seq.next_element()? else { break };
                entries.push(entry);
            }
            Ok(entries)
        }
    }
    decoder.deserialize_seq(Four(std::marker::PhantomData))
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Field {
    Text {
        id: String,
        label: String,
    },
    Select {
        id: String,
        label: String,
        #[serde(deserialize_with = "four")]
        choices: Vec<String>,
    },
    MultiSelect {
        id: String,
        label: String,
        #[serde(deserialize_with = "four")]
        choices: Vec<String>,
    },
    Confirm {
        id: String,
        label: String,
    },
    Integer {
        id: String,
        label: String,
        min: i64,
        max: i64,
    },
}

fn text(value: &str, cap: usize) -> bool {
    !value.is_empty()
        && value.len() <= cap
        && !value
            .chars()
            .any(|c| c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
}
fn id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 48
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

impl Field {
    fn parts(&self) -> (&str, &str) {
        match self {
            Self::Text { id, label }
            | Self::Select { id, label, .. }
            | Self::MultiSelect { id, label, .. }
            | Self::Confirm { id, label }
            | Self::Integer { id, label, .. } => (id, label),
        }
    }
    fn valid(&self) -> bool {
        let (key, label) = self.parts();
        if !id(key) || !text(label, 256) {
            return false;
        }
        match self {
            Self::Select { choices, .. } | Self::MultiSelect { choices, .. } => {
                !choices.is_empty()
                    && choices.iter().all(|c| text(c, 256))
                    && choices.iter().enumerate().all(|(at, c)| !choices[..at].contains(c))
            }
            Self::Integer { min, max, .. } => {
                min <= max && *min >= -9_007_199_254_740_991 && *max <= 9_007_199_254_740_991
            }
            _ => true,
        }
    }
    fn question(&self, caption: &str) -> Question {
        let (_, label) = self.parts();
        let (choices, multi) = match self {
            Self::Select { choices, .. } => (choices.clone(), false),
            Self::MultiSelect { choices, .. } => (choices.clone(), true),
            Self::Confirm { .. } => (vec!["Yes".into(), "No".into()], false),
            _ => (Vec::new(), false),
        };
        let hint = match self {
            Self::Integer { min, max, .. } => format!(" ({min}..={max})"),
            Self::Select { .. } | Self::MultiSelect { .. } => " (choose listed options)".into(),
            Self::Confirm { .. } => " (Yes or No)".into(),
            _ => String::new(),
        };
        Question { question: format!("{caption}\n{label}{hint}"), choices, multi }
    }
    fn value(&self, answer: &Answer) -> Option<serde_json::Value> {
        let values = &answer.chosen;
        if values.is_empty() || values.len() > 4 || !values.iter().all(|s| text(s, 1024)) {
            return None;
        }
        match self {
            Self::Text { .. } if values.len() == 1 => Some(values[0].clone().into()),
            Self::Integer { min, max, .. } if values.len() == 1 => {
                values[0].trim().parse::<i64>().ok().filter(|n| min <= n && n <= max).map(Into::into)
            }
            Self::Confirm { .. } if values.len() == 1 => match values[0].as_str() {
                "Yes" => Some(true.into()),
                "No" => Some(false.into()),
                _ => None,
            },
            Self::Select { choices, .. } if values.len() == 1 && choices.contains(&values[0]) => {
                Some(values[0].clone().into())
            }
            Self::MultiSelect { choices, .. }
                if values.iter().all(|v| choices.contains(v))
                    && values.iter().enumerate().all(|(at, v)| !values[..at].contains(v)) =>
            {
                Some(serde_json::json!(values))
            }
            _ => None,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct Reply {
    pub id: String,
    pub status: &'static str,
    pub values: Option<std::collections::BTreeMap<String, serde_json::Value>>,
}

impl Form {
    pub(crate) fn parse(raw: &str, settings: &Settings) -> std::io::Result<Self> {
        if raw.len() > settings.max_update_bytes {
            return Err(std::io::Error::other("form exceeds update byte limit"));
        }
        let form: Self = serde_json::from_str(raw)?;
        if !id(&form.id)
            || !text(&form.title, 256)
            || form.fields.is_empty()
            || !form.fields.iter().all(Field::valid)
            || !form
                .fields
                .iter()
                .enumerate()
                .all(|(at, f)| !form.fields[..at].iter().any(|p| p.parts().0 == f.parts().0))
        {
            return Err(std::io::Error::other("invalid extension form"));
        }
        Ok(form)
    }
    pub(crate) fn report(&self, source: &Source, status: &'static str) -> Batch {
        Batch {
            source: source.clone(),
            items: vec![Item::Status {
                id: format!("form.{}", self.id),
                text: format!("{} · {status}", self.title),
            }],
        }
    }
    pub(crate) async fn ask(&self, source: &Source, settings: &Settings, asker: Option<&dyn Asker>) -> Reply {
        let Some(asker) = asker else {
            return self.reply("unavailable", None);
        };
        let caption = format!(
            "Extension hook {} #{} · source {} · form {}\n{}",
            source.event.as_str(),
            source.ordinal.saturating_add(1),
            source.digest.get(..8).unwrap_or("unknown"),
            self.id,
            self.title
        );
        let questions: Vec<_> = self.fields.iter().map(|f| f.question(&caption)).collect();
        let answers = asker.ask(&questions).await;
        if answers.len() != self.fields.len() || answers.iter().any(|a| a.chosen.is_empty()) {
            return self.reply("unanswered", None);
        }
        let values = self
            .fields
            .iter()
            .zip(&answers)
            .map(|(field, answer)| field.value(answer).map(|v| (field.parts().0.to_string(), v)))
            .collect::<Option<std::collections::BTreeMap<_, _>>>();
        if values.is_none() {
            return self.reply("invalid", None);
        }
        let reply = self.reply("answered", values);
        // An extension gets only an admitted encoded answer, including escaping.
        if super::encoded(&reply, settings.max_update_bytes).is_err() {
            return self.reply("invalid", None);
        }
        reply
    }
    fn reply(
        &self,
        status: &'static str,
        values: Option<std::collections::BTreeMap<String, serde_json::Value>>,
    ) -> Reply {
        Reply { id: self.id.clone(), status, values }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Chooses(Vec<Vec<String>>);
    #[async_trait::async_trait]
    impl Asker for Chooses {
        async fn ask(&self, questions: &[Question]) -> Vec<Answer> {
            assert!(questions.iter().all(|q| q.question.contains("Extension hook prompt #1 · source")
                && q.question.contains("form setup")));
            questions
                .iter()
                .enumerate()
                .map(|(at, q)| Answer {
                    question: q.question.clone(),
                    chosen: self.0.get(at).cloned().unwrap_or_default(),
                })
                .collect()
        }
    }
    fn source() -> Source {
        Source::hook(
            &crate::hooks::HookConfig {
                event: crate::hooks::Event::Prompt,
                command: "fixture".into(),
                ..Default::default()
            },
            0,
        )
    }
    const FORM: &str = r#"{"id":"setup","title":"Project target","fields":[{"kind":"text","id":"name","label":"Name"},{"kind":"select","id":"target","label":"Target","choices":["local","remote"]},{"kind":"confirm","id":"confirm","label":"Continue"},{"kind":"integer","id":"count","label":"Count","min":1,"max":10}]}"#;
    #[tokio::test]
    async fn typed_answers_are_exact_and_unavailable_unanswered_invalid_are_distinct() {
        let settings = Settings::default();
        let form = Form::parse(FORM, &settings).unwrap();
        let picks = |values: &[&str]| Chooses(values.iter().map(|v| vec![(*v).into()]).collect());
        let result = form.ask(&source(), &settings, Some(&picks(&["name", "local", "No", "7"]))).await;
        assert_eq!(result.status, "answered");
        assert_eq!(
            serde_json::to_value(result.values).unwrap(),
            serde_json::json!({"name":"name","target":"local","confirm":false,"count":7})
        );
        assert_eq!(form.ask(&source(), &settings, None).await.status, "unavailable");
        assert_eq!(form.ask(&source(), &settings, Some(&Chooses(vec![]))).await.status, "unanswered");
        for values in [
            ["name", "unknown", "No", "7"],
            ["name", "local", "maybe", "7"],
            ["name", "local", "No", "7.1"],
            ["name", "local", "No", "11"],
        ] {
            let result = form.ask(&source(), &settings, Some(&picks(&values))).await;
            assert_eq!(result.status, "invalid");
            assert!(result.values.is_none());
        }
        let huge =
            Chooses(vec![vec!["x".repeat(1025)], vec!["local".into()], vec!["No".into()], vec!["7".into()]]);
        assert_eq!(form.ask(&source(), &settings, Some(&huge)).await.status, "invalid");
    }
    #[test]
    fn schema_bounds_before_extra_entries_and_rejects_ambiguous_or_forged_forms() {
        let settings = Settings::default();
        let base: serde_json::Value = serde_json::from_str(FORM).unwrap();
        let mut extra = base.clone();
        extra["fields"].as_array_mut().unwrap().push(base["fields"][0].clone());
        let mut duplicate = base.clone();
        duplicate["fields"][1]["id"] = "name".into();
        let mut choices = base.clone();
        choices["fields"][1]["choices"] = serde_json::json!(["a", "b", "c", "d", "e"]);
        let mut source = base.clone();
        source["source"] = "trusted".into();
        let mut number = base.clone();
        number["fields"][3]["max"] = 9007199254740992i64.into();
        for value in [extra, duplicate, choices, source, number] {
            assert!(Form::parse(&value.to_string(), &settings).is_err());
        }
        assert!(Form::parse(&" ".repeat(settings.max_update_bytes + 1), &settings).is_err());
    }
    #[tokio::test]
    async fn multi_select_and_encoded_answer_budget_are_admitted_before_retention() {
        let settings = Settings::default();
        let raw = r#"{"id":"setup","title":"Options","fields":[{"kind":"multi_select","id":"items","label":"Choose","choices":["a","b","c"]}]}"#;
        let form = Form::parse(raw, &settings).unwrap();
        let chosen = Chooses(vec![vec!["b".into(), "a".into()]]);
        assert_eq!(
            form.ask(&source(), &settings, Some(&chosen)).await.values.unwrap()["items"],
            serde_json::json!(["b", "a"])
        );
        let duplicate = Chooses(vec![vec!["a".into(), "a".into()]]);
        assert_eq!(form.ask(&source(), &settings, Some(&duplicate)).await.status, "invalid");
        let raw = r#"{"id":"setup","title":"Text","fields":[{"kind":"text","id":"x","label":"X"}]}"#;
        let form = Form::parse(raw, &settings).unwrap();
        let lower = Settings { max_update_bytes: 1024, ..settings };
        let escaped = Chooses(vec![vec!["\"".repeat(900)]]);
        assert_eq!(form.ask(&source(), &lower, Some(&escaped)).await.status, "invalid");
    }
}
