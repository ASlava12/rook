use super::*;
use std::collections::VecDeque;
#[derive(Default)]
pub(crate) struct Pool(std::sync::Mutex<VecDeque<(String, rook_llm::Proxy, reqwest::Client)>>);
impl Pool {
    pub(super) fn client(
        &self,
        target: &reqwest::Url,
        proxy: &rook_llm::Proxy,
        limit: usize,
    ) -> Result<reqwest::Client> {
        let mut cache = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(at) = cache.iter().position(|(url, via, _)| url == target.as_str() && via == proxy) {
            let entry = cache.remove(at).expect("located HTTP client");
            let client = entry.2.clone();
            cache.push_back(entry);
            return Ok(client);
        }
        rook_llm::init_tls();
        let builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(std::time::Duration::from_secs(10));
        let client = proxy
            .on(builder, Some(target.as_str()))
            .map_err(|_| bad("invalid remote agent proxy"))?
            .build()
            .map_err(|_| bad("could not prepare remote agent HTTP transport"))?;
        while cache.len() >= limit.clamp(1, 32) {
            cache.pop_front();
        }
        cache.push_back((target.as_str().into(), proxy.clone(), client.clone()));
        Ok(client)
    }
}
pub(super) async fn response(
    request: impl std::future::Future<Output = std::result::Result<reqwest::Response, reqwest::Error>>,
    limit: usize,
    route: &str,
    delivery: &str,
    timestamp: &str,
    secret: &[u8],
    request_hash: &[u8],
) -> Result<Value> {
    let mut response = request.await.map_err(|_| bad("transport failed before a confirmed reply"))?;
    let status = response.status();
    let signed = response
        .headers()
        .get("x-rook-response-signature")
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.len() == 67 && value.starts_with("v1="))
        .map(str::to_owned);
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| bad("reply body was interrupted"))? {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err(bad("reply exceeded response_bytes while arriving"));
        }
        bytes.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        return Err(bad(format!("HTTP {} (remote request rejected or unconfirmed)", status.as_u16())));
    }
    let signed = signed.ok_or_else(|| bad("peer response signature is missing or malformed"))?;
    let code = hex::decode(&signed[3..]).map_err(|_| bad("invalid peer response signature"))?;
    use hmac::Mac;
    let mac = reply_mac(route, delivery, timestamp, request_hash, &bytes, secret)?;
    mac.verify_slice(&code).map_err(|_| bad("peer response signature did not authenticate these bytes"))?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| bad("peer returned invalid bounded JSON"))?;
    if value["protocol"] != "rook-a2a-v1" || !value["result"].is_object() {
        return Err(bad("peer did not confirm the Rook A2A protocol"));
    }
    Ok(value["result"].clone())
}
