//! OAuth discovery and grants. Network errors never carry response bodies or URLs.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{StatusCode, Url};
use rook_mcp::ServerConfig;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(super) type Result<T> = std::result::Result<T, &'static str>;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Issuer {
    pub issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    #[serde(default)]
    registration_endpoint: Option<String>,
    #[serde(default, deserialize_with = "bounded_list::<_, 64>")]
    code_challenge_methods_supported: Vec<String>,
    #[serde(default, deserialize_with = "optional_list")]
    token_endpoint_auth_methods_supported: Option<Vec<String>>,
    #[serde(default)]
    authorization_response_iss_parameter_supported: bool,
}
#[derive(Deserialize)]
struct Resource {
    resource: String,
    #[serde(deserialize_with = "bounded_list::<_, 32>")]
    authorization_servers: Vec<String>,
    #[serde(default, deserialize_with = "bounded_list::<_, 64>")]
    scopes_supported: Vec<String>,
}

pub(super) struct Discovery {
    pub issuer: Issuer,
    pub resource: String,
    pub scopes: Vec<String>,
}

/// Private values: neither Debug nor a frontend-serializable login status.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Grant {
    pub resource: String,
    pub issuer: String,
    pub client_id: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<u64>,
    pub scope: Option<String>,
}
impl Grant {
    pub fn valid(&self) -> bool {
        valid_token(&self.access_token)
            && self.refresh_token.as_deref().is_none_or(valid_token)
            && !self.client_id.is_empty()
            && self.client_id.len() <= 2048
            && !self.client_id.chars().any(char::is_control)
            && safe_url(&self.issuer).is_ok()
            && safe_url(&self.resource).is_ok()
            && self.scope.as_deref().is_none_or(|s| scopes(s).is_ok())
    }
    pub fn usable(&self, now: u64) -> bool {
        self.expires_at.is_none_or(|expires| expires > now)
    }
}

pub(super) struct Pending {
    pub discovery: Discovery,
    pub client_id: String,
    pub redirect: String,
    state: String,
    verifier: String,
}

impl Pending {
    pub fn accepts(&self, callback: &str) -> bool {
        Url::parse(callback).is_ok_and(|url| {
            url.query_pairs().filter(|(key, _)| key == "state").collect::<Vec<_>>()
                == [("state".into(), self.state.as_str().into())]
        })
    }
}

pub(super) struct Client<'a> {
    config: &'a ServerConfig,
    proxy: &'a rook_llm::Proxy,
}
impl<'a> Client<'a> {
    pub fn new(config: &'a ServerConfig, proxy: &'a rook_llm::Proxy) -> Result<Self> {
        if let Some(error) = config.oauth.error().or_else(|| config.catalog_error()) {
            return Err(error);
        }
        let url = config.url.as_deref().ok_or("OAuth requires an HTTP MCP server")?;
        safe_url(url)?;
        if config.headers.keys().any(|key| key.eq_ignore_ascii_case("authorization")) {
            return Err("remove the configured Authorization header before using MCP OAuth");
        }
        Ok(Self { config, proxy })
    }

    fn http(&self, target: &str) -> Result<reqwest::Client> {
        let endpoint = safe_url(target)?;
        let resource = safe_url(self.config.url.as_deref().ok_or("OAuth requires an HTTP MCP server")?)?;
        if endpoint.scheme() == "http" && resource.scheme() != "http" {
            return Err("remote OAuth discovery cannot redirect credentials to an HTTP loopback service");
        }
        rook_llm::init_tls();
        let builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(std::time::Duration::from_secs(15));
        self.proxy
            .on(builder, Some(target))
            .map_err(|_| "invalid OAuth proxy configuration")?
            .build()
            .map_err(|_| "could not create OAuth client")
    }

    async fn json<T: for<'de> Deserialize<'de>>(&self, mut response: reqwest::Response) -> Result<T> {
        if !response.status().is_success() {
            return Err("OAuth endpoint refused the request; check client registration and sign in again");
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| "OAuth response was interrupted")? {
            if chunk.len() > self.config.oauth.max_response_bytes.saturating_sub(bytes.len()) {
                return Err("OAuth response exceeds oauth.max_response_bytes");
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| "invalid OAuth response")
    }

    async fn metadata<T: for<'de> Deserialize<'de>>(&self, urls: Vec<String>) -> Result<T> {
        for url in urls {
            let response = self
                .http(&url)?
                .get(&url)
                .send()
                .await
                .map_err(|_| "OAuth metadata endpoint is unavailable")?;
            if matches!(response.status(), StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED) {
                continue;
            }
            return self.json(response).await;
        }
        Err("OAuth discovery metadata was not found; check this server's OAuth support")
    }

    pub async fn discover(&self) -> Result<Discovery> {
        let target = self.config.url.as_deref().ok_or("OAuth requires an HTTP MCP server")?;
        let resource_url = safe_url(target)?;
        let mut probe = self.http(target)?.post(target).header("accept", "application/json, text/event-stream")
            .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":rook_mcp::PROTOCOL_VERSION,"capabilities":{},"clientInfo":{"name":"rook","version":env!("CARGO_PKG_VERSION")}}}));
        for (key, value) in &self.config.headers {
            probe = probe.header(key, value);
        }
        let response = probe.send().await.map_err(|_| "MCP server is unavailable for OAuth discovery")?;
        let challenge = if response.status() == StatusCode::UNAUTHORIZED {
            challenge(
                response
                    .headers()
                    .get_all(reqwest::header::WWW_AUTHENTICATE)
                    .iter()
                    .filter_map(|v| v.to_str().ok()),
            )?
        } else {
            Challenge::default()
        };
        drop(response);
        let paths = match challenge.metadata {
            Some(url) => vec![url],
            None => well_known(&resource_url, "oauth-protected-resource", true),
        };
        let resource: Resource = self.metadata(paths).await?;
        // The resource indicator is never allowed to move a grant to another
        // origin/path. Root discovery is a location fallback, not a new target.
        if resource.resource != target {
            return Err("OAuth resource metadata does not match the configured MCP URL");
        }
        if resource.authorization_servers.is_empty() || resource.authorization_servers.len() > 32 {
            return Err("OAuth resource must advertise between 1 and 32 authorization servers");
        }
        let wanted = if self.config.oauth.issuer.is_empty() {
            &resource.authorization_servers[0]
        } else {
            &self.config.oauth.issuer
        };
        if !resource.authorization_servers.contains(wanted) {
            return Err("configured OAuth issuer is not advertised by this MCP resource");
        }
        let issuer = self.issuer(wanted).await?;
        let scopes = match challenge.scope {
            Some(scope) => scopes(&scope)?,
            None if !self.config.oauth.scopes.is_empty() => self.config.oauth.scopes.clone(),
            None => resource.scopes_supported,
        };
        validate_scopes(&scopes)?;
        Ok(Discovery { issuer, resource: target.into(), scopes })
    }

    async fn issuer(&self, expected: &str) -> Result<Issuer> {
        let url = safe_url(expected)?;
        if url.query().is_some() {
            return Err("OAuth issuer must not contain a query");
        }
        let mut locations = well_known(&url, "oauth-authorization-server", false);
        locations.extend(well_known(&url, "openid-configuration", false));
        if url.path() != "/" {
            locations.push(format!("{}/.well-known/openid-configuration", expected.trim_end_matches('/')));
        }
        let issuer: Issuer = self.metadata(locations).await?;
        if issuer.issuer != expected {
            return Err("OAuth metadata issuer does not match its discovery URL");
        }
        let authorization = safe_url(&issuer.authorization_endpoint)?;
        let token = safe_url(&issuer.token_endpoint)?;
        if !issuer.code_challenge_methods_supported.iter().any(|method| method == "S256") {
            return Err("OAuth server does not advertise S256 PKCE; sign-in refused");
        }
        if issuer
            .token_endpoint_auth_methods_supported
            .as_ref()
            .is_some_and(|methods| !methods.iter().any(|m| m == "none"))
        {
            return Err(
                "OAuth server requires a confidential client; register a public PKCE client for Rook",
            );
        }
        if !issuer.authorization_response_iss_parameter_supported
            && authorization.origin() != url.origin()
            && authorization.origin() != token.origin()
        {
            return Err(
                "OAuth authorization endpoint cannot be bound to its issuer without issuer-aware callbacks",
            );
        }
        Ok(issuer)
    }

    pub async fn prepare(&self, redirect: String) -> Result<(Pending, String)> {
        callback_url(&redirect)?;
        let discovery = self.discover().await?;
        let client_id = if self.config.oauth.client_id.is_empty() {
            let registration = discovery.issuer.registration_endpoint.as_deref().ok_or("set oauth.client_id for a public PKCE client; this issuer does not offer dynamic registration")?;
            let response = self.http(registration)?.post(registration).json(&serde_json::json!({
                "client_name":"Rook", "redirect_uris":[redirect], "grant_types":["authorization_code","refresh_token"],
                "response_types":["code"], "token_endpoint_auth_method":"none"
            })).send().await.map_err(|_| "OAuth client registration failed")?;
            #[derive(Deserialize)]
            struct Registered {
                client_id: String,
                #[serde(default)]
                token_endpoint_auth_method: Option<String>,
            }
            let registered: Registered = self.json(response).await?;
            if registered.token_endpoint_auth_method.as_deref() != Some("none") {
                return Err("OAuth registration did not create a public client");
            }
            registered.client_id
        } else {
            self.config.oauth.client_id.clone()
        };
        if client_id.is_empty() || client_id.len() > 2048 || client_id.chars().any(char::is_control) {
            return Err("invalid OAuth client ID");
        }
        let state = random()?;
        let verifier = random()?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut authorize = safe_url(&discovery.issuer.authorization_endpoint)?;
        let reserved = [
            "client_id",
            "redirect_uri",
            "response_type",
            "state",
            "code_challenge",
            "code_challenge_method",
            "resource",
            "scope",
        ];
        if authorize.query_pairs().any(|(key, _)| reserved.contains(&key.as_ref())) {
            return Err("OAuth authorization endpoint predefines reserved parameters");
        }
        {
            let mut query = authorize.query_pairs_mut();
            query
                .append_pair("response_type", "code")
                .append_pair("client_id", &client_id)
                .append_pair("redirect_uri", &redirect)
                .append_pair("state", &state)
                .append_pair("code_challenge", &challenge)
                .append_pair("code_challenge_method", "S256")
                .append_pair("resource", &discovery.resource);
            if !discovery.scopes.is_empty() {
                query.append_pair("scope", &discovery.scopes.join(" "));
            }
        }
        Ok((Pending { discovery, client_id, redirect, state, verifier }, authorize.into()))
    }

    pub async fn exchange(&self, pending: Pending, callback: &str) -> Result<Grant> {
        let callback = Url::parse(callback).map_err(|_| "invalid OAuth callback")?;
        let mut base = callback.clone();
        base.set_query(None);
        base.set_fragment(None);
        if base.as_str() != pending.redirect || callback.fragment().is_some() {
            return Err("OAuth callback address does not match");
        }
        let mut pairs = std::collections::BTreeMap::new();
        for (key, value) in callback.query_pairs() {
            if pairs.insert(key.into_owned(), value.into_owned()).is_some() {
                return Err("duplicate OAuth callback parameter");
            }
        }
        if pairs.get("state") != Some(&pending.state) {
            return Err("OAuth callback state does not match");
        }
        let issuer = &pending.discovery.issuer;
        if pairs.get("iss").is_some_and(|value| value != &issuer.issuer)
            || (issuer.authorization_response_iss_parameter_supported && !pairs.contains_key("iss"))
        {
            return Err("OAuth callback issuer does not match; no code was exchanged");
        }
        if pairs.contains_key("error") {
            return Err("OAuth authorization was declined or failed; start sign-in again");
        }
        let code = pairs
            .get("code")
            .filter(|code| !code.is_empty() && code.len() <= 16384)
            .ok_or("OAuth callback has no valid code")?;
        let body = form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", &pending.client_id),
            ("redirect_uri", &pending.redirect),
            ("code_verifier", &pending.verifier),
            ("resource", &pending.discovery.resource),
        ])?;
        self.grant(issuer, &pending.discovery.resource, &pending.client_id, body, None).await
    }

    pub async fn refresh_metadata(&self, old: &Grant) -> Result<Discovery> {
        let discovery = self.discover().await?;
        if discovery.resource != old.resource || discovery.issuer.issuer != old.issuer {
            return Err("OAuth issuer/resource changed; sign in again before sending a refresh token");
        }
        Ok(discovery)
    }

    pub async fn refresh(&self, old: &Grant, discovery: &Discovery) -> Result<Grant> {
        let refresh = old
            .refresh_token
            .as_deref()
            .ok_or("OAuth access expired without a refresh token; sign in again")?;
        let body = form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh),
            ("client_id", &old.client_id),
            ("resource", &old.resource),
        ])?;
        self.grant(&discovery.issuer, &old.resource, &old.client_id, body, Some(old)).await
    }

    async fn grant(
        &self,
        issuer: &Issuer,
        resource: &str,
        client_id: &str,
        body: String,
        old: Option<&Grant>,
    ) -> Result<Grant> {
        // Count network time against the lifetime too. A fixed early-refresh
        // margin would discard legitimate tokens with a shorter lifetime.
        let requested_at = now();
        let response = self
            .http(&issuer.token_endpoint)?
            .post(&issuer.token_endpoint)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .map_err(|_| "OAuth token request failed; sign in again")?;
        #[derive(Deserialize)]
        struct Tokens {
            access_token: String,
            token_type: String,
            refresh_token: Option<String>,
            expires_in: Option<u64>,
            scope: Option<String>,
        }
        let tokens: Tokens = self.json(response).await?;
        if !tokens.token_type.eq_ignore_ascii_case("bearer")
            || !valid_token(&tokens.access_token)
            || tokens.refresh_token.as_deref().is_some_and(|t| !valid_token(t))
        {
            return Err("invalid OAuth token response");
        }
        if tokens.expires_in == Some(0) {
            return Err("OAuth server returned an already expired token");
        }
        if let Some(scope) = &tokens.scope {
            scopes(scope)?;
        }
        let grant = Grant {
            resource: resource.into(),
            issuer: issuer.issuer.clone(),
            client_id: client_id.into(),
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token.or_else(|| old.and_then(|g| g.refresh_token.clone())),
            expires_at: tokens.expires_in.map(|s| requested_at.saturating_add(s)),
            scope: tokens.scope.or_else(|| old.and_then(|g| g.scope.clone())),
        };
        if !grant.usable(now()) {
            return Err("OAuth token expired while its response was arriving; sign in again");
        }
        Ok(grant)
    }
}

pub(super) fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()
}
fn valid_token(token: &str) -> bool {
    !token.is_empty() && token.len() <= 16384 && token.bytes().all(|b| b.is_ascii_graphic())
}
pub(super) fn random() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| "OS randomness is unavailable; OAuth cannot start")?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
pub(super) fn callback_url(text: &str) -> Result<Url> {
    let url = Url::parse(text).map_err(|_| "invalid OAuth callback URL")?;
    // Metadata destinations remain stricter. This address belongs to a
    // frontend whose browser may have opened the daemon as localhost.
    let localhost = url.scheme() == "http"
        && url.host_str() == Some("localhost")
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && text.len() <= 4096;
    if !localhost {
        safe_url(text)?;
    }
    if url.query().is_some() {
        return Err("OAuth callback must not contain a query");
    }
    Ok(url)
}
pub(super) fn safe_url(text: &str) -> Result<Url> {
    if text.len() > 4096 {
        return Err("OAuth URL is too long");
    }
    let url = Url::parse(text).map_err(|_| "invalid OAuth URL")?;
    let loopback = url.host_str().is_some_and(|h| {
        h.trim_matches(['[', ']']).parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
    });
    if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "OAuth URLs require HTTPS (or a numeric loopback address), with no credentials or fragments",
        );
    }
    Ok(url)
}
fn well_known(url: &Url, suffix: &str, root_fallback: bool) -> Vec<String> {
    let root = url.origin().ascii_serialization();
    let path = url.path().trim_end_matches('/');
    let mut locations = vec![format!("{root}/.well-known/{suffix}{path}")];
    if root_fallback && !path.is_empty() {
        locations.push(format!("{root}/.well-known/{suffix}"));
    }
    locations
}
fn form(pairs: &[(&str, &str)]) -> Result<String> {
    let mut url = Url::parse("http://127.0.0.1/").map_err(|_| "could not encode OAuth form")?;
    url.query_pairs_mut().extend_pairs(pairs.iter().copied());
    Ok(url.query().unwrap_or_default().into())
}
fn scopes(text: &str) -> Result<Vec<String>> {
    let mut values = Vec::new();
    for scope in text.split(' ').filter(|s| !s.is_empty()) {
        if values.len() == 64 {
            return Err("OAuth scope list exceeds 64 entries");
        }
        values.push(scope.to_owned());
    }
    validate_scopes(&values)?;
    Ok(values)
}

fn validate_scopes(values: &[String]) -> Result<()> {
    if values.len() > 64
        || values.iter().any(|v| {
            v.is_empty()
                || v.len() > 256
                || !v.bytes().all(|b| b == 0x21 || (0x23..=0x5b).contains(&b) || (0x5d..=0x7e).contains(&b))
        })
    {
        return Err("OAuth scope list exceeds its limits or contains invalid values");
    }
    Ok(())
}

fn bounded_list<'de, D: serde::Deserializer<'de>, const N: usize>(
    deserializer: D,
) -> std::result::Result<Vec<String>, D::Error> {
    struct List<const N: usize>;
    impl<'de, const N: usize> serde::de::Visitor<'de> for List<N> {
        type Value = Vec<String>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "at most {N} bounded strings")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            use serde::de::Error;
            let mut values = Vec::new();
            while values.len() < N {
                let Some(value) = seq.next_element::<String>()? else { return Ok(values) };
                if value.len() > 4096 {
                    return Err(A::Error::custom("OAuth metadata string too long"));
                }
                values.push(value);
            }
            if seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
                return Err(A::Error::custom("too many OAuth metadata entries"));
            }
            Ok(values)
        }
    }
    deserializer.deserialize_seq(List::<N>)
}
fn optional_list<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Vec<String>>, D::Error> {
    struct Optional;
    impl<'de> serde::de::Visitor<'de> for Optional {
        type Value = Option<Vec<String>>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("optional bounded OAuth methods")
        }
        fn visit_none<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_some<D: serde::Deserializer<'de>>(self, d: D) -> std::result::Result<Self::Value, D::Error> {
            bounded_list::<D, 64>(d).map(Some)
        }
    }
    deserializer.deserialize_option(Optional)
}

#[derive(Default)]
struct Challenge {
    metadata: Option<String>,
    scope: Option<String>,
}
fn challenge<'a>(headers: impl Iterator<Item = &'a str>) -> Result<Challenge> {
    let mut out = Challenge::default();
    let mut total = 0usize;
    for header in headers {
        total = total.saturating_add(header.len());
        if total > 16384 {
            return Err("OAuth authentication challenge exceeds 16 KiB");
        }
        let mut parts = Vec::new();
        let mut start = 0;
        let mut quoted = false;
        let mut escaped = false;
        for (at, ch) in header.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            if quoted && ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == '"' {
                quoted = !quoted;
            }
            if ch == ',' && !quoted {
                parts.push(&header[start..at]);
                start = at + 1;
            }
        }
        if quoted || escaped {
            return Err("malformed OAuth authentication challenge");
        }
        parts.push(&header[start..]);
        let mut bearer = false;
        for part in parts {
            let mut param = part.trim();
            if let Some((scheme, rest)) = param.split_once(' ')
                && !scheme.contains('=')
                && !rest.trim_start().starts_with('=')
            {
                bearer = scheme.eq_ignore_ascii_case("bearer");
                param = rest.trim();
            } else if !param.contains('=') {
                bearer = param.eq_ignore_ascii_case("bearer");
                continue;
            }
            if !bearer {
                continue;
            }
            let Some((key, raw)) = param.split_once('=') else {
                return Err("malformed OAuth challenge parameter");
            };
            let slot = if key.trim().eq_ignore_ascii_case("resource_metadata") {
                &mut out.metadata
            } else if key.trim().eq_ignore_ascii_case("scope") {
                &mut out.scope
            } else {
                continue;
            };
            let raw = raw.trim();
            let value = if raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2 {
                let mut value = String::new();
                let mut chars = raw[1..raw.len() - 1].chars();
                while let Some(ch) = chars.next() {
                    value.push(if ch == '\\' {
                        chars.next().ok_or("malformed OAuth quoted value")?
                    } else {
                        ch
                    });
                }
                value
            } else if raw.bytes().all(|b| b.is_ascii_graphic() && b != b'"') {
                raw.into()
            } else {
                return Err("malformed OAuth challenge value");
            };
            if slot.as_ref().is_some_and(|previous| previous != &value) {
                return Err("ambiguous OAuth authentication challenges");
            }
            *slot = Some(value);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_lists_refuse_overflow_during_deserialization() {
        let many = vec!["https://issuer.example"; 33];
        assert!(many.len() > 32);
        let encoded = serde_json::json!({"resource":"https://example.com/mcp","authorization_servers":many});
        assert!(serde_json::from_value::<Resource>(encoded).is_err());
        assert!(scopes(&vec!["read"; 65].join(" ")).is_err());
    }
    #[test]
    fn header_challenges_respect_quoted_commas_and_refuse_ambiguity() {
        let parsed=challenge([r#"Basic realm="a,b", Bearer resource_metadata="https://example.com/meta?a,b", scope="read write""#].into_iter()).unwrap();
        assert_eq!(parsed.metadata.as_deref(), Some("https://example.com/meta?a,b"));
        assert_eq!(parsed.scope.as_deref(), Some("read write"));
        assert!(
            challenge(
                [
                    r#"Bearer resource_metadata="https://first.example""#,
                    r#"Bearer resource_metadata="https://second.example""#
                ]
                .into_iter()
            )
            .is_err()
        );
        let oversized = "x".repeat(16385);
        assert!(oversized.len() > 16384);
        assert!(challenge([oversized.as_str()].into_iter()).is_err());
    }
    #[test]
    fn a_remote_resource_cannot_send_oauth_requests_to_cleartext_loopback() {
        let config = ServerConfig { url: Some("https://public.example/mcp".into()), ..Default::default() };
        let proxy = rook_llm::Proxy::default();
        assert!(Client::new(&config, &proxy).unwrap().http("http://127.0.0.1/metadata").is_err());
    }
}
