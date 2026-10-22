//! Bounded recovery of interrupted release bodies, before any installation.

use super::{Asset, MOST_ASSET_BYTES, MOST_HOPS, host_of};
use reqwest::header::{ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_RANGE, LOCATION, RANGE};
use sha2::Digest;
use std::time::Duration;

const ATTEMPTS: usize = 3;
const PATIENCE: Duration = Duration::from_secs(600);

pub(super) async fn fetch_asset(client: &reqwest::Client, release: &Asset) -> Result<Vec<u8>, String> {
    with_patience(client, release, PATIENCE).await
}

pub(super) async fn with_patience(
    client: &reqwest::Client,
    release: &Asset,
    patience: Duration,
) -> Result<Vec<u8>, String> {
    if release.size > MOST_ASSET_BYTES as u64 {
        return Err(format!("the release asset exceeds the {MOST_ASSET_BYTES}-byte limit — refused"));
    }
    tokio::time::timeout(patience, fetch_with_retries(client, release)).await.map_err(|_| {
        format!("the asset download exceeded {} seconds — nothing was installed", patience.as_secs_f64())
    })?
}

enum Failure {
    Interrupted(String),
    Refused(String),
}

async fn response(
    client: &reqwest::Client,
    release: &Asset,
    offset: usize,
) -> Result<reqwest::Response, Failure> {
    let mut at = release.url.clone();
    let origin = host_of(&at);
    for _ in 0..MOST_HOPS {
        let mut request = client.get(&at).header(ACCEPT_ENCODING, "identity");
        if offset > 0 {
            request = request.header(RANGE, format!("bytes={offset}-"));
        }
        let hop = request.send().await.map_err(|error| {
            Failure::Interrupted(format!("could not fetch the asset: {}", error.without_url()))
        })?;
        let Some(location) = hop.headers().get(LOCATION) else {
            return Ok(hop);
        };
        let to = location
            .to_str()
            .map_err(|_| Failure::Refused("the asset redirect is not a valid URL — refused".into()))?
            .to_string();
        let host = host_of(&to);
        if host != origin
            && host != "objects.githubusercontent.com"
            && host != "release-assets.githubusercontent.com"
        {
            return Err(Failure::Refused("the asset redirects outside its release hosts — refused".into()));
        }
        at = to;
    }
    Err(Failure::Refused(format!("the asset redirected more than {MOST_HOPS} times — refused")))
}

fn range(value: &str) -> Option<(u64, u64, u64)> {
    let (unit, value) = value.trim().split_once(' ')?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }
    let (span, total) = value.split_once('/')?;
    let (start, end) = span.split_once('-')?;
    let decimal = |value: &str| {
        (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| value.parse::<u64>().ok())
            .flatten()
    };
    let (start, end, total) = (decimal(start)?, decimal(end)?, decimal(total)?);
    (start <= end && end < total).then_some((start, end, total))
}

fn admit(
    response: &reqwest::Response,
    release: &Asset,
    offset: usize,
    body: &mut Vec<u8>,
    hasher: &mut sha2::Sha256,
    total: &mut Option<u64>,
) -> Result<(), String> {
    if !response.status().is_success() {
        return Err(format!("the asset answered {} — refused", response.status()));
    }
    if response
        .headers()
        .get_all(CONTENT_ENCODING)
        .iter()
        .any(|value| !value.to_str().is_ok_and(|text| text.trim().eq_ignore_ascii_case("identity")))
    {
        return Err("the asset has an unexpected content encoding — refused".into());
    }
    let remaining = if response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
        let mut values = response.headers().get_all(CONTENT_RANGE).iter();
        let parsed = values.next().and_then(|value| value.to_str().ok()).and_then(range);
        let (start, end, length) = parsed.ok_or("the asset Content-Range is invalid — refused")?;
        if values.next().is_some()
            || offset == 0
            || start != offset as u64
            || end != length - 1
            || total.is_some_and(|expected| expected != length)
            || length > MOST_ASSET_BYTES as u64
        {
            return Err("the asset Content-Range does not match the requested remainder — refused".into());
        }
        *total = Some(length);
        Some(length - start)
    } else {
        if response.headers().contains_key(CONTENT_RANGE) {
            return Err("the full asset response unexpectedly carries Content-Range — refused".into());
        }
        // A server may ignore Range. Restart rather than joining two complete bodies.
        body.clear();
        *hasher = sha2::Sha256::new();
        *total = (release.size != 0).then_some(release.size).or_else(|| response.content_length());
        *total
    };
    if response.content_length().is_some_and(|length| length > MOST_ASSET_BYTES as u64)
        || total.is_some_and(|length| length > MOST_ASSET_BYTES as u64)
    {
        return Err(format!("the asset exceeds the {MOST_ASSET_BYTES}-byte limit — refused"));
    }
    if let (Some(length), Some(expected)) = (response.content_length(), remaining)
        && length != expected
    {
        return Err("the asset length does not match what the release says — refused".into());
    }
    Ok(())
}

async fn fetch_with_retries(client: &reqwest::Client, release: &Asset) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    let mut hasher = sha2::Sha256::new();
    let mut total = (release.size != 0).then_some(release.size);
    let mut interrupted = String::new();
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        // Keep the original requirement for a completed HTTP body. If all bytes
        // arrived but framing failed, obtain a fresh full response, not bytes=N-.
        if !body.is_empty() && total.is_some_and(|length| body.len() as u64 >= length) {
            body.clear();
            hasher = sha2::Sha256::new();
            total = (release.size != 0).then_some(release.size);
        }
        let offset = body.len();
        let mut response = match response(client, release, offset).await {
            Ok(response) => response,
            Err(Failure::Refused(why)) => return Err(why),
            Err(Failure::Interrupted(why)) => {
                interrupted = why;
                continue;
            }
        };
        admit(&response, release, offset, &mut body, &mut hasher, &mut total)?;
        let mut complete = false;
        loop {
            match response.chunk().await {
                Ok(None) => {
                    complete = true;
                    break;
                }
                Ok(Some(chunk)) => {
                    if chunk.len() > MOST_ASSET_BYTES.saturating_sub(body.len()) {
                        return Err(format!("the asset exceeds the {MOST_ASSET_BYTES}-byte limit — refused"));
                    }
                    if total.is_some_and(|length| (body.len() + chunk.len()) as u64 > length) {
                        return Err("the asset body exceeds its declared length — refused".into());
                    }
                    hasher.update(&chunk);
                    body.extend_from_slice(&chunk);
                }
                Err(error) => {
                    interrupted = format!("the body stopped partway: {}", error.without_url());
                    break;
                }
            }
        }
        if complete {
            if total.is_some_and(|length| (body.len() as u64) < length) {
                interrupted = "the body ended before the declared asset size".into();
                continue;
            }
            let actual = hex::encode(hasher.finalize());
            if actual != release.sha256 {
                return Err(format!(
                    "the download does not match the digest the release lists: got sha256 {actual}, \
                     the release says {} — nothing was installed",
                    release.sha256
                ));
            }
            return Ok(body);
        }
    }
    Err(format!(
        "the download stopped partway after {ATTEMPTS} attempts ({} bytes received in this invocation): \
         {interrupted} — nothing was installed",
        body.len()
    ))
}
