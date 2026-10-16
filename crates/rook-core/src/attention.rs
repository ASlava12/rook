//! Attention is a read-only projection, never authority to answer saved input.
use crate::{CoreError, Result, Rook};
use rook_proto::activity::Stage;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Query {
    pub after: Option<String>,
    pub limit: Option<usize>,
}
impl Query {
    pub fn cursor(&self) -> Result<Option<u128>> {
        self.after
            .as_deref()
            .map(|value| {
                rook_store::parse_session_id(value)
                    .ok_or_else(|| CoreError::Other("attention cursor must be a session ID".into()))
            })
            .transpose()
    }
    pub fn count(&self) -> usize {
        self.limit.unwrap_or(64).clamp(1, 256)
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Live {
    pub generation: String,
    pub questions: usize,
    pub approvals: usize,
    pub secrets: usize,
}
impl Live {
    fn waiting(&self) -> bool {
        self.questions + self.approvals + self.secrets > 0
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Item {
    pub session: String,
    pub parent: Option<String>,
    pub title: String,
    pub live: Option<Live>,
    pub saved_stage: Option<Stage>,
    /// False when no owner registry is available. Saved hints never grant answer authority.
    pub owner_checked: bool,
    pub depth: usize,
    pub prefix: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page {
    pub items: Vec<Item>,
    pub next: Option<String>,
    pub scanned: usize,
}
impl Rook {
    /// Scan a bounded metadata page, joining only current owner-supplied counts.
    /// No question bodies, secret aliases or answer controls are copied here.
    pub fn attention_page(
        &self,
        query: &Query,
        owner_checked: bool,
        live: impl Fn(u128) -> Option<Live>,
    ) -> Result<Page> {
        let ids = self.store.session_ids_after(query.cursor()?, query.count() + 1)?;
        self.project_attention(ids, query.count(), owner_checked, live)
    }

    /// Live owners are indexed separately from historical sessions, so a recent
    /// question cannot be hidden behind thousands of empty history pages.
    pub fn live_attention_page(
        &self,
        query: &Query,
        live: &std::collections::BTreeMap<u128, Live>,
    ) -> Result<Page> {
        let after = query.cursor()?;
        let ids = live
            .iter()
            .filter(|(id, state)| after.is_none_or(|after| **id > after) && state.waiting())
            .take(query.count() + 1)
            .map(|(id, _)| *id)
            .collect();
        self.project_attention(ids, query.count(), true, |id| live.get(&id).cloned())
    }

    fn project_attention(
        &self,
        mut ids: Vec<u128>,
        limit: usize,
        owner_checked: bool,
        live: impl Fn(u128) -> Option<Live>,
    ) -> Result<Page> {
        let more = ids.len() > limit;
        ids.truncate(limit);
        let next = more.then(|| ids.last().copied()).flatten().map(rook_store::format_session_id);
        let mut items = Vec::new();
        let mut summaries = Vec::new();
        for id in &ids {
            let live = live(*id).filter(Live::waiting);
            let saved_stage = if owner_checked {
                None
            } else {
                crate::activity::load(&self.store, *id)?
                    .filter(|s| matches!(s.stage, Stage::Approval | Stage::Question))
                    .map(|s| s.stage)
            };
            if live.is_none() && (owner_checked || saved_stage.is_none()) {
                continue;
            }
            let Some(meta) = self.store.get_session(*id)? else { continue };
            items.push(Item {
                session: rook_store::format_session_id(*id),
                parent: meta.parent.map(rook_store::format_session_id),
                title: meta.title.chars().filter(|c| !c.is_control()).take(72).collect(),
                live,
                saved_stage,
                owner_checked,
                depth: 0,
                prefix: String::new(),
            });
            summaries.push(crate::SessionSummary { meta, goal: None, forked_at: None });
        }
        let items = crate::session_list::rows(&summaries)
            .into_iter()
            .map(|row| {
                let mut item = items[row.index].clone();
                item.depth = row.depth;
                item.prefix = row.prefix;
                item
            })
            .collect();
        Ok(Page { items, next, scanned: ids.len() })
    }
}
/// Shared text rendering for command and terminal views. Reattachment resolves current requests.
pub fn describe(page: &Page) -> String {
    let mut text = String::new();
    for item in &page.items {
        let state = match &item.live {
            Some(live) => format!(
                "{} question(s), {} permission(s), {} private input(s)",
                live.questions, live.approvals, live.secrets
            ),
            None => "saved waiting observation; current owner unknown".into(),
        };
        text.push_str(&format!(
            "{}{} · {} · {}{}\n",
            item.prefix,
            item.session,
            item.title,
            state,
            item.parent.as_ref().map(|p| format!(" · child of {p}")).unwrap_or_default()
        ));
    }
    if page.items.is_empty() {
        text.push_str("No waiting requests in this scanned page.\n");
    }
    text.push_str(
        "Open/resume the exact session to resolve current requests. Skipping or declining is allowed.\n",
    );
    if let Some(next) = &page.next {
        text.push_str(&format!("More sessions: /attention {next} (CLI: --after {next})\n"));
    }
    text
}
