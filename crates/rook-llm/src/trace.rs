//! Bounded correlation metadata. No prompts, credentials or exporter.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceContext {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
}

impl TraceContext {
    pub fn bounded(trace: &str, span: &str, parent: Option<&str>) -> Option<Self> {
        fn valid(value: &str, bytes: usize) -> bool {
            value.len() == bytes
                && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                && value.bytes().any(|b| b != b'0')
        }
        (valid(trace, 32) && valid(span, 16) && parent.is_none_or(|p| valid(p, 16))).then(|| Self {
            trace_id: trace.into(),
            span_id: span.into(),
            parent_span_id: parent.map(str::to_owned),
        })
    }

    pub fn traceparent(&self) -> Option<String> {
        Self::bounded(&self.trace_id, &self.span_id, self.parent_span_id.as_deref())?;
        Some(format!("00-{}-{}-01", self.trace_id, self.span_id))
    }
}

/// Cloneable only inside one physical attempt; each admission gets fresh state.
#[derive(Clone, Debug, Default)]
pub struct RequestTrace {
    context: Option<TraceContext>,
    propagate: bool,
    ids: Arc<Mutex<BTreeMap<String, String>>>,
}
impl RequestTrace {
    pub fn new(context: Option<(TraceContext, bool)>) -> Self {
        match context {
            Some((context, propagate)) => Self { context: Some(context), propagate, ..Default::default() },
            None => Self::default(),
        }
    }

    pub(crate) fn apply(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.propagate
            && let Some(value) = self.context.as_ref().and_then(TraceContext::traceparent)
        {
            return request.header("traceparent", value);
        }
        request
    }

    pub(crate) fn response(&self, headers: &reqwest::header::HeaderMap, credential: Option<&str>) {
        let mut ids = self.ids.lock().unwrap_or_else(|e| e.into_inner());
        for name in ["x-request-id", "request-id", "x-amzn-requestid", "x-goog-request-id", "cf-ray"] {
            if ids.len() >= 4 {
                break;
            }
            let Some(value) = headers.get(name).and_then(|v| v.to_str().ok()) else { continue };
            if valid_request_id(value)
                && !credential.is_some_and(|key| !key.is_empty() && value.contains(key))
            {
                ids.insert(name.into(), value.into());
            }
        }
    }

    pub(crate) fn request_ids(&self) -> BTreeMap<String, String> {
        self.ids.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

pub fn valid_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.:/".contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_metadata_reaches_its_count_and_byte_limits_without_retaining_other_headers() {
        let trace = RequestTrace::default();
        let mut headers = reqwest::header::HeaderMap::new();
        for name in ["x-request-id", "request-id", "x-amzn-requestid", "x-goog-request-id", "cf-ray"] {
            headers.insert(name, "valid-id".parse().unwrap());
        }
        assert_eq!(headers.len(), 5);
        trace.response(&headers, None);
        assert_eq!(trace.request_ids().len(), 4);
        let edge = "a".repeat(128);
        let overflow = "a".repeat(129);
        assert_eq!(edge.len(), 128);
        assert!(overflow.len() > 128);
        assert!(valid_request_id(&edge));
        assert!(!valid_request_id(&overflow));
        for invalid in ["", "token with spaces", "line\nbreak", "ключ"] {
            assert!(!valid_request_id(invalid));
        }
        assert!(TraceContext::bounded(&"0".repeat(32), &"1".repeat(16), None).is_none());
        assert!(TraceContext::bounded(&"a".repeat(32), &"0".repeat(16), None).is_none());
    }
}
