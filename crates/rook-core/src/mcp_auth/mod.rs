//! Browser authorization and private, resource-bound MCP credentials.
mod protocol;
mod store;

use crate::mcp_connections::Settings;
use protocol::{Client, Grant, Result, now};
use rook_mcp::{ServerConfig, oauth::TokenSource};
use std::sync::Arc;
use store::{Entry, Store, key};

pub(crate) fn credentials(
    config: &ServerConfig,
    proxy: &rook_llm::Proxy,
    limits: Settings,
) -> Result<Option<Arc<dyn TokenSource>>> {
    if config.url.is_none() || config.headers.keys().any(|h| h.eq_ignore_ascii_case("authorization")) {
        return Ok(None);
    }
    let store = Store::configured(limits);
    credentials_from(config, proxy, store)
}
fn credentials_from(
    config: &ServerConfig,
    proxy: &rook_llm::Proxy,
    store: Store,
) -> Result<Option<Arc<dyn TokenSource>>> {
    let Some(entry) = store.read()?.into_iter().find(|entry| entry.key == key(config)) else {
        return Ok(None);
    };
    Client::new(config, proxy)?;
    if Some(&entry.grant.resource) != config.url.as_ref()
        || (!config.oauth.issuer.is_empty() && entry.grant.issuer != config.oauth.issuer)
        || (!config.oauth.client_id.is_empty() && entry.grant.client_id != config.oauth.client_id)
    {
        return Err("stored OAuth grant does not match this MCP configuration; sign in again");
    }
    Ok(Some(Arc::new(Credentials {
        config: config.clone(),
        proxy: proxy.clone(),
        store,
        snapshot: tokio::sync::Mutex::new(entry),
    })))
}
struct Credentials {
    config: ServerConfig,
    proxy: rook_llm::Proxy,
    store: Store,
    snapshot: tokio::sync::Mutex<Entry>,
}
#[async_trait::async_trait]
impl TokenSource for Credentials {
    async fn token(&self) -> Result<String> {
        let mut snapshot = self.snapshot.lock().await;
        let locate = |entries: Vec<Entry>| {
            entries
                .into_iter()
                .find(|entry| entry.key == snapshot.key)
                .ok_or("MCP OAuth credentials were removed; sign in again")
        };
        let current = locate(self.store.read()?)?;
        if current.epoch != snapshot.epoch {
            // An account switch must not move an already-approved turn onto
            // the new identity. Only a new connection takes up the new grant.
            return if snapshot.grant.usable(now()) {
                Ok(snapshot.grant.access_token.clone())
            } else {
                Err("MCP OAuth identity changed; start a new turn with the reconnected server")
            };
        }
        if current.grant.usable(now()) {
            *snapshot = current;
            return Ok(snapshot.grant.access_token.clone());
        }
        let _guard = self.store.lock(self.config.oauth.timeout_secs).await?;
        let mut entries = self.store.read()?;
        let entry = entries
            .iter_mut()
            .find(|entry| entry.key == snapshot.key)
            .ok_or("MCP OAuth credentials were removed; sign in again")?;
        if entry.epoch != snapshot.epoch {
            return Err("MCP OAuth identity changed; use the new connection");
        }
        if entry.grant.usable(now()) {
            *snapshot = entry.clone();
            return Ok(entry.grant.access_token.clone());
        }
        if entry.refreshing {
            return Err(
                "previous OAuth refresh did not finish safely; sign in again instead of retrying its token",
            );
        }
        if entry.grant.refresh_token.is_none() {
            return Err("OAuth access expired; sign in again");
        }
        let old = entry.grant.clone();
        let client = Client::new(&self.config, &self.proxy)?;
        // Discovery sends no refresh token, so its failures are safe to retry.
        let metadata = tokio::time::timeout(
            std::time::Duration::from_secs(self.config.oauth.timeout_secs),
            client.refresh_metadata(&old),
        )
        .await
        .map_err(|_| "OAuth refresh discovery timed out")??;
        entry.refreshing = true;
        self.store.write(&entries)?;
        let fresh = tokio::time::timeout(
            std::time::Duration::from_secs(self.config.oauth.timeout_secs),
            client.refresh(&old, &metadata),
        )
        .await
        .map_err(|_| "OAuth refresh timed out; sign in again")??;
        let entry = entries
            .iter_mut()
            .find(|entry| entry.key == snapshot.key)
            .ok_or("OAuth credential entry disappeared")?;
        entry.grant = fresh;
        entry.refreshing = false;
        let updated = entry.clone();
        self.store.write(&entries)?;
        *snapshot = updated;
        Ok(snapshot.grant.access_token.clone())
    }
}

struct FixedToken(String);
#[async_trait::async_trait]
impl TokenSource for FixedToken {
    async fn token(&self) -> Result<String> {
        Ok(self.0.clone())
    }
}

/// Start with a real native loopback callback. The URL is public authorization
/// material; access/refresh tokens are never passed to the frontend callback.
pub async fn login_native(
    config: &ServerConfig,
    proxy: &rook_llm::Proxy,
    limits: Settings,
    show_url: impl FnOnce(&str) + Send,
) -> Result<()> {
    login_at(config, proxy, Store::configured(limits), show_url).await
}

/// Resolve the same trusted, bounded declarations as managed reconnect.
pub fn configured(
    workspace: &std::path::Path,
    name: &str,
) -> Result<(ServerConfig, rook_llm::Proxy, Settings)> {
    let config = crate::Config::load_for(workspace)
        .map_err(|_| "could not load configuration; run rook config check")?;
    let (plugins, errors) = crate::plugins::discover(workspace);
    if !errors.is_empty() {
        return Err("could not load trusted MCP plugin declarations");
    }
    let servers = crate::mcp_connections::admitted(
        config.mcp.iter().chain(plugins.iter().flat_map(|p| &p.mcp)),
        config.mcp_connections,
    )?;
    let server = servers
        .into_iter()
        .find(|s| s.name == name)
        .ok_or("MCP server is not configured; add it with rook config edit")?;
    if !server.enabled {
        return Err("MCP server is disabled; enable it before signing in");
    }
    Ok((server.clone(), config.proxy.for_mcp(), config.mcp_connections))
}

/// One consent attempt. Dropping it cancels the attempt without storing a grant.
/// Pending codes and PKCE material deliberately have no Debug/Serialize surface.
/// The callback may belong to a native listener or the daemon's browser origin.
pub struct Login {
    config: ServerConfig,
    proxy: rook_llm::Proxy,
    store: Store,
    pending: protocol::Pending,
    url: String,
    deadline: tokio::time::Instant,
}

impl Login {
    /// `redirect` is a callback owned by the frontend, never an OAuth-server URL.
    pub async fn begin(
        config: &ServerConfig,
        proxy: &rook_llm::Proxy,
        limits: Settings,
        redirect: String,
    ) -> Result<Self> {
        Self::begin_at(config, proxy, Store::configured(limits), redirect).await
    }

    async fn begin_at(
        config: &ServerConfig,
        proxy: &rook_llm::Proxy,
        store: Store,
        redirect: String,
    ) -> Result<Self> {
        if let Some(error) = store.limits.error() {
            return Err(error);
        }
        protocol::callback_url(&redirect)?;
        let client = Client::new(config, proxy)?;
        let entries = store.read()?;
        if entries.len() >= store.limits.oauth_max_entries && !entries.iter().any(|e| e.key == key(config)) {
            return Err("MCP credential store is full; sign out unused servers before signing in");
        }
        let (pending, url) = tokio::time::timeout(
            std::time::Duration::from_secs(config.oauth.timeout_secs),
            client.prepare(redirect),
        )
        .await
        .map_err(|_| "OAuth discovery timed out")??;
        Ok(Self {
            config: config.clone(),
            proxy: proxy.clone(),
            store,
            pending,
            url,
            deadline: tokio::time::Instant::now()
                + std::time::Duration::from_secs(config.oauth.login_timeout_secs),
        })
    }

    /// Do not finish a browser flow after its server declaration was edited.
    pub fn matches_config(&self, config: &ServerConfig) -> bool {
        &self.config == config
    }

    /// Only the authorization URL is safe to show in the frontend.
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn is_expired(&self) -> bool {
        tokio::time::Instant::now() >= self.deadline
    }

    /// A wrong state does not consume a pending attempt. Full callback and
    /// issuer validation still happen in `complete` before exchanging a code.
    pub fn accepts(&self, callback: &str) -> bool {
        !self.is_expired() && callback.len() <= 32 * 1024 && self.pending.accepts(callback)
    }

    pub async fn complete(self, callback: &str) -> Result<()> {
        if self.is_expired() {
            return Err("OAuth sign-in expired; start it again");
        }
        if callback.len() > 32 * 1024 {
            return Err("OAuth callback exceeds 32 KiB");
        }
        tokio::time::timeout_at(self.deadline, async {
            let client = Client::new(&self.config, &self.proxy)?;
            let grant = tokio::time::timeout(
                std::time::Duration::from_secs(self.config.oauth.timeout_secs),
                client.exchange(self.pending, callback),
            )
            .await
            .map_err(|_| "OAuth code exchange timed out; start sign-in again")??;
            validate_grant(&self.config, &self.proxy, &grant).await?;
            self.store.save(&self.config, grant).await
        })
        .await
        .map_err(|_| "OAuth sign-in expired; start it again")?
    }
}

async fn login_at(
    config: &ServerConfig,
    proxy: &rook_llm::Proxy,
    store: Store,
    show_url: impl FnOnce(&str) + Send,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, config.oauth.callback_port))
        .await
        .map_err(|_| "could not bind the OAuth callback port; choose another oauth.callback_port")?;
    let address = listener.local_addr().map_err(|_| "could not read OAuth callback address")?;
    let redirect = format!("http://{address}/callback");
    let login = Login::begin_at(config, proxy, store, redirect).await?;
    show_url(login.url());
    tokio::time::timeout_at(login.deadline, async {
        let (mut socket, callback) = loop {
            let (mut socket, _) = listener.accept().await.map_err(|_| "OAuth callback listener failed")?;
            let path = callback(&mut socket, &address.to_string()).await;
            let callback = path.map(|path| format!("http://{address}{path}"));
            if let Ok(callback) = callback
                && login.accepts(&callback)
            {
                break (socket, callback);
            }
            reply(&mut socket, false).await;
        };
        let result = login.complete(&callback).await;
        reply(&mut socket, result.is_ok()).await;
        result
    })
    .await
    .map_err(|_| "OAuth sign-in expired; start it again")?
}

async fn validate_grant(config: &ServerConfig, proxy: &rook_llm::Proxy, grant: &Grant) -> Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(config.oauth.timeout_secs), async {
        let token: Arc<dyn TokenSource> = Arc::new(FixedToken(grant.access_token.clone()));
        let server = rook_mcp::Server::connect_with_token(config, proxy, Some(token)).await.map_err(
            |_| "MCP server did not accept the new OAuth grant; previous credentials are unchanged",
        )?;
        let result =
            server.list_tools().await.map(|_| ()).map_err(
                |_| "MCP catalog failed with the new OAuth grant; previous credentials are unchanged",
            );
        server.shutdown().await;
        result
    })
    .await
    .map_err(|_| "MCP verification timed out; previous OAuth credentials are unchanged")?
}

pub async fn logout(config: &ServerConfig, limits: Settings) -> Result<bool> {
    let store = Store::configured(limits);
    let _guard = store.lock(config.oauth.timeout_secs).await?;
    let mut entries = store.read()?;
    let before = entries.len();
    entries.retain(|entry| entry.server_name != config.name);
    if entries.len() != before {
        store.write(&entries)?;
    }
    Ok(entries.len() != before)
}

async fn callback(socket: &mut tokio::net::TcpStream, authority: &str) -> Result<String> {
    use tokio::io::AsyncReadExt;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let n = socket.read(&mut chunk).await.map_err(|_| "could not read OAuth callback")?;
            if n == 0 {
                return Err("OAuth callback closed early");
            }
            if n > (32 * 1024usize).saturating_sub(bytes.len()) {
                return Err("OAuth callback exceeds 32 KiB");
            }
            bytes.extend_from_slice(&chunk[..n]);
            if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                let headers =
                    std::str::from_utf8(&bytes[..end]).map_err(|_| "invalid OAuth callback encoding")?;
                let mut lines = headers.lines();
                let request = lines.next().ok_or("missing OAuth callback request")?;
                let mut words = request.split(' ');
                if words.next() != Some("GET") {
                    return Err("OAuth callback requires GET");
                }
                let path = words
                    .next()
                    .filter(|p| p.starts_with("/callback?"))
                    .ok_or("invalid OAuth callback path")?;
                if words.next() != Some("HTTP/1.1") || words.next().is_some() {
                    return Err("invalid OAuth callback request");
                }
                let hosts: Vec<_> = lines
                    .filter_map(|line| line.split_once(':'))
                    .filter(|(name, _)| name.eq_ignore_ascii_case("host"))
                    .collect();
                if hosts.len() != 1 || hosts[0].1.trim() != authority {
                    return Err("invalid OAuth callback host");
                }
                return Ok(path.into());
            }
        }
    })
    .await
    .map_err(|_| "OAuth callback read timed out")?
}
async fn reply(socket: &mut tokio::net::TcpStream, success: bool) {
    use tokio::io::AsyncWriteExt;
    let (status, text) = if success {
        ("200 OK", "Rook sign-in finished. Return to Rook.")
    } else {
        ("400 Bad Request", "Rook could not complete this sign-in. Return to Rook for details.")
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'\r\nConnection: close\r\n\r\n{text}",
        text.len()
    );
    let _ =
        tokio::time::timeout(std::time::Duration::from_secs(5), socket.write_all(response.as_bytes())).await;
}

#[cfg(test)]
mod tests;
