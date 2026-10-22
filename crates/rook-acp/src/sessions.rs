//! Stable id cursors keep discovery bounded even when most sessions belong to
//! another workspace. Empty filtered pages may still carry a continuation.

use rook_core::Rook;
use serde::Deserialize;

use crate::protocol::Error;

const PAGE_SESSIONS: usize = 64;
const SCAN_SESSIONS: usize = 512;
const META_BYTES: usize = 64 * 1024;

#[derive(Default, Deserialize)]
struct List {
    cwd: Option<String>,
    cursor: Option<String>,
}

pub(crate) fn list(rook: &Rook, params: serde_json::Value) -> Result<serde_json::Value, Error> {
    if !params.is_null() && !params.is_object() {
        return Err(Error::invalid_params("session/list parameters must be an object or null"));
    }
    let request: List = if params.is_null() {
        List::default()
    } else {
        serde_json::from_value(params).map_err(|error| Error::invalid_params(error.to_string()))?
    };
    if let Some(cwd) = &request.cwd
        && (cwd.len() > 4096 || !std::path::Path::new(cwd).is_absolute())
    {
        return Err(Error::invalid_params("cwd must be an absolute path of at most 4096 bytes"));
    }
    // Session setup canonicalizes cwd. Match Windows verbatim paths and Unix
    // aliases the same way, while retaining discovery of a removed directory.
    let cwd = request.cwd.as_ref().map(|cwd| {
        let path = std::path::PathBuf::from(cwd);
        path.canonicalize().unwrap_or(path)
    });
    let after = request
        .cursor
        .as_deref()
        .map(|cursor| {
            rook_store::parse_session_id(cursor)
                .ok_or_else(|| Error::invalid_params("invalid session-list cursor"))
        })
        .transpose()?;
    let ids = rook
        .store
        .session_ids_after(after, SCAN_SESSIONS + 1)
        .map_err(|error| Error::internal(error.to_string()))?;
    let mut sessions = Vec::new();
    let mut last = after;
    let mut scanned = 0;
    for &id in ids.iter().take(SCAN_SESSIONS) {
        last = Some(id);
        scanned += 1;
        let meta = rook
            .store
            .get_session_limited(id, META_BYTES)
            .map_err(|error| Error::internal(error.to_string()))?;
        let Some(meta) = meta else { continue };
        if meta.workspace.len() > 4096 {
            return Err(Error::internal("stored session workspace exceeds 4096 bytes"));
        }
        if let Some(cwd) = &cwd {
            let stored = std::path::Path::new(&meta.workspace);
            if cwd != stored && !stored.canonicalize().is_ok_and(|stored| &stored == cwd) {
                continue;
            }
        }
        sessions.push(serde_json::json!({
            "sessionId": rook_store::format_session_id(meta.id),
            "title": rook_tools::elide_middle(&meta.title, 1024),
            "cwd": meta.workspace,
        }));
        if sessions.len() == PAGE_SESSIONS {
            break;
        }
    }
    let mut result = serde_json::json!({ "sessions": sessions });
    if ids.len() > scanned
        && let Some(last) = last
    {
        result["nextCursor"] = rook_store::format_session_id(last).into();
    }
    Ok(result)
}

/// Close validates identity before it cancels any connection-owned turn.
pub(crate) fn existing(rook: &Rook, params: &serde_json::Value) -> Result<u128, Error> {
    let session = params["sessionId"]
        .as_str()
        .filter(|id| id.len() <= 64)
        .ok_or_else(|| Error::invalid_params("sessionId must be a string of at most 64 bytes"))?;
    let id =
        rook_store::parse_session_id(session).ok_or_else(|| Error::invalid_params("not a session id"))?;
    match rook.store.get_session_limited(id, META_BYTES) {
        Ok(Some(_)) => Ok(id),
        Ok(None) => Err(Error::invalid_params("no such session")),
        Err(error) => Err(Error::internal(error.to_string())),
    }
}
