//! Per-command HTTP/CONNECT egress authority. Socket containment is mandatory;
//! proxy variables alone are never treated as an enforced domain boundary.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    /// Exact host:port or *.suffix:port. No implicit public or local exceptions.
    pub allow: Vec<String>,
    /// Named hosts resolving to private addresses need this opt-in. Explicit IP
    /// rules already identify the intended address and do not need it.
    pub allow_private: bool,
    pub max_connections: usize,
    pub max_transfer_bytes: u64,
    pub idle_timeout_secs: u64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            allow: vec![],
            allow_private: false,
            max_connections: 8,
            max_transfer_bytes: 32 * 1024 * 1024,
            idle_timeout_secs: 60,
        }
    }
}
#[derive(Clone)]
struct Target {
    host: String,
    port: u16,
}
fn authority(text: &str) -> Result<Target, String> {
    if text.len() > 256
        || !text.is_ascii()
        || text.bytes().any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
        || text.contains(['@', '/', '?', '#', '\\'])
    {
        return Err("expected a bounded host:port authority".into());
    }
    let (host, port) = text.rsplit_once(':').ok_or("an explicit port is required")?;
    let host = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host).to_ascii_lowercase();
    let port = port.parse::<u16>().ok().filter(|p| *p != 0).ok_or("invalid port")?;
    if host.parse::<IpAddr>().is_err()
        && (host.is_empty()
            || host.len() > 253
            || host.split('.').any(|label| {
                label.is_empty()
                    || label.len() > 63
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            }))
    {
        return Err("invalid hostname".into());
    }
    Ok(Target { host, port })
}
impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if self.allow.len() > 64
            || !(1..=32).contains(&self.max_connections)
            || !(1024..=64 * 1024 * 1024).contains(&self.max_transfer_bytes)
            || !(1..=300).contains(&self.idle_timeout_secs)
        {
            return Err("sandbox.network_policy: at most 64 rules, connections 1..=32, transfer bytes 1024..=67108864 and idle seconds 1..=300".into());
        }
        for rule in &self.allow {
            let target = authority(rule.strip_prefix("*.").unwrap_or(rule))
                .map_err(|e| format!("sandbox.network_policy.allow: {e}"))?;
            if rule.starts_with("*.") && (target.host.parse::<IpAddr>().is_ok() || !target.host.contains('.'))
            {
                return Err("network wildcard needs a DNS suffix with at least two labels".into());
            }
        }
        Ok(())
    }
    fn permits(&self, target: &Target) -> bool {
        self.allow.iter().any(|rule| {
            let wildcard = rule.starts_with("*.");
            authority(rule.strip_prefix("*.").unwrap_or(rule)).is_ok_and(|r| {
                r.port == target.port
                    && if wildcard {
                        target
                            .host
                            .strip_suffix(&r.host)
                            .is_some_and(|prefix| prefix.ends_with('.') && prefix.len() > 1)
                    } else {
                        r.host == target.host
                    }
            })
        })
    }
}
fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.is_unspecified()
                || ip.is_multicast()
                || a == 0
                || a >= 240
                || (a == 100 && (64..=127).contains(&b))
                || (a == 198 && (b == 18 || b == 19))
                || (a == 192 && b == 0 && c == 0))
        }
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map(|ip| public(ip.into())).unwrap_or_else(|| {
            let segments = ip.segments();
            (segments[0] & 0xe000) == 0x2000
                && !(segments[0] == 0x2001 && (segments[1] < 0x200 || segments[1] == 0xdb8))
                && segments[0] != 0x2002
        }),
    }
}
#[derive(Default)]
struct Audit {
    decisions: VecDeque<Value>,
    omitted: u64,
}
impl Audit {
    fn record(&mut self, target: Option<&Target>, decision: &str, reason: &str) {
        if self.decisions.len() == 32 {
            self.decisions.pop_front();
            self.omitted += 1;
        }
        self.decisions.push_back(json!({"host":target.map(|t|t.host.as_str()),"port":target.map(|t|t.port),"decision":decision,"reason":reason}));
    }
}
/// Held with the command's stdin guard. Drop retires the listener and all tunnels.
pub(crate) struct Proxy {
    pub port: u16,
    id: String,
    audit: Arc<Mutex<Audit>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Proxy {
    pub fn report(&self) -> Value {
        let audit = self.audit.lock().unwrap_or_else(|e| e.into_inner());
        json!({"id":self.id,"enforcement":"seatbelt_proxy_only","decisions":audit.decisions,"omitted":audit.omitted,"scope":"this foreground command and its descendants"})
    }
    pub fn start(settings: &Settings) -> std::io::Result<Self> {
        settings.validate().map_err(std::io::Error::other)?;
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;
        let listener = TcpListener::from_std(listener)?;
        let audit = Arc::new(Mutex::new(Audit::default()));
        let log = audit.clone();
        let settings = settings.clone();
        let task = tokio::spawn(async move {
            let mut children = tokio::task::JoinSet::new();
            let mut accepted = 0;
            loop {
                tokio::select! {
                    _=children.join_next(), if !children.is_empty()=>{},
                    next=listener.accept()=>{
                        let Ok((mut stream,_))=next else {break};
                        if children.len()>=settings.max_connections || accepted>=256 {
                            log.lock().unwrap_or_else(|e|e.into_inner()).record(None,"deny","connection bound");
                            // No task or growing queue for refused connections.
                            let _=stream.try_write(b"HTTP/1.1 503 Service Unavailable\r\nConnection: close\r\nContent-Length: 0\r\n\r\n");
                            continue;
                        }
                        accepted+=1;let settings=settings.clone();let log=log.clone();
                        children.spawn(async move {
                            if let Err(reason)=serve(&mut stream,&settings,&log).await {
                                record(&log,None,"stop",&reason);
                                let _=tokio::time::timeout(Duration::from_secs(1),stream.write_all(b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")).await;
                            }
                        });
                    }
                }
            }
        });
        Ok(Self { port, id: ulid::Ulid::generate().to_string(), audit, task })
    }
}
fn record(audit: &Mutex<Audit>, target: Option<&Target>, decision: &str, reason: &str) {
    audit.lock().unwrap_or_else(|e| e.into_inner()).record(target, decision, reason);
}
async fn addresses(target: &Target, settings: &Settings) -> Result<Vec<SocketAddr>, String> {
    if let Ok(ip) = target.host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, target.port)]);
    }
    let mut addresses = Vec::new();
    let mut lookup =
        tokio::net::lookup_host((target.host.as_str(), target.port)).await.map_err(|_| "DNS failed")?;
    for address in lookup.by_ref().take(17) {
        if addresses.len() == 16 {
            return Err("DNS address bound".into());
        }
        if !settings.allow_private && !public(address.ip()) {
            return Err("DNS resolved to a private or special address".into());
        }
        addresses.push(address);
    }
    if addresses.is_empty() {
        return Err("DNS returned no addresses".into());
    }
    Ok(addresses)
}
async fn serve(stream: &mut TcpStream, settings: &Settings, audit: &Mutex<Audit>) -> Result<(), String> {
    let idle = Duration::from_secs(settings.idle_timeout_secs);
    let header = tokio::time::timeout(idle, async {
        let mut header = Vec::new();
        // Reading exactly the header prevents discarding TLS bytes pipelined
        // after CONNECT, and applies the cap before allocating the next byte.
        loop {
            if header.len() >= 8192 {
                return Err("header bound");
            };
            let b = stream.read_u8().await.map_err(|_| "incomplete header")?;
            header.push(b);
            if header.ends_with(b"\r\n\r\n") {
                return Ok(header);
            }
        }
    })
    .await
    .map_err(|_| "header idle timeout")?
    .inspect_err(|e| {
        record(audit, None, "deny", e);
    })?;
    let header = std::str::from_utf8(&header).map_err(|_| "invalid header")?;
    let mut lines = header.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split(' ');
    let method = first.next().unwrap_or_default();
    let uri = first.next().unwrap_or_default();
    let version = first.next().unwrap_or_default();
    if !["HTTP/1.0", "HTTP/1.1"].contains(&version) || first.next().is_some() {
        record(audit, None, "deny", "invalid request line");
        return Err("invalid request line".into());
    }
    let connect = method == "CONNECT";
    let url = if connect { None } else { Some(reqwest::Url::parse(uri).map_err(|_| "invalid URL")?) };
    let target = if let Some(url) = &url {
        if url.scheme() != "http"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err("HTTP proxy requires a plain HTTP URL without userinfo".into());
        }
        let host = url.host_str().ok_or("missing host")?;
        authority(&format!("{host}:{}", url.port_or_known_default().ok_or("missing port")?))?
    } else {
        authority(uri)?
    };
    if !settings.permits(&target) {
        record(audit, Some(&target), "deny", "no matching host:port rule");
        return Err("destination denied".into());
    }
    let mut headers = Vec::new();
    let mut length = None;
    let mut host = false;
    for line in lines.filter(|line| !line.is_empty()) {
        let (key, value) = line.split_once(':').ok_or("invalid header field")?;
        if key.is_empty()
            || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || value.bytes().any(|b| b.is_ascii_control() && b != b'\t')
        {
            return Err("invalid header field".into());
        }
        let key = key.to_ascii_lowercase();
        let value = value.trim();
        match key.as_str() {
            "content-length" => {
                if length.is_some() {
                    return Err("duplicate length".into());
                };
                length = Some(value.parse::<usize>().map_err(|_| "invalid length")?);
            }
            "transfer-encoding" | "upgrade" => {
                return Err("request transfer coding/upgrade unsupported".into());
            }
            "host" => {
                if host {
                    return Err("duplicate host".into());
                };
                host = true;
            }
            "connection"
            | "proxy-connection"
            | "proxy-authorization"
            | "keep-alive"
            | "te"
            | "trailer"
            | "expect" => {}
            _ => headers.push((key, value.to_owned())),
        }
    }
    let length = length.unwrap_or(0);
    if length > 1024 * 1024 || (connect && length != 0) {
        return Err("request body bound".into());
    }
    let addresses = tokio::time::timeout(Duration::from_secs(10), addresses(&target, settings))
        .await
        .map_err(|_| "DNS timeout")?
        .inspect_err(|reason| record(audit, Some(&target), "deny", reason))?;
    if connect {
        let mut remote = None;
        for address in &addresses {
            if let Ok(Ok(stream)) =
                tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(address)).await
            {
                remote = Some(stream);
                break;
            }
        }
        let mut remote = remote.ok_or_else(|| {
            record(audit, Some(&target), "error", "connect failed");
            "connect failed"
        })?;
        record(audit, Some(&target), "allow", "pinned CONNECT destination");
        tokio::time::timeout(idle, stream.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n"))
            .await
            .map_err(|_| "write timeout")?
            .map_err(|_| "write failed")?;
        let (mut cr, mut cw) = stream.split();
        let (mut rr, mut rw) = remote.split();
        let activity = Arc::new(Mutex::new(tokio::time::Instant::now()));
        let a = pump(&mut cr, &mut rw, idle, settings.max_transfer_bytes, &activity);
        let b = pump(&mut rr, &mut cw, idle, settings.max_transfer_bytes, &activity);
        let watchdog = async {
            loop {
                let last = *activity.lock().unwrap_or_else(|e| e.into_inner());
                tokio::time::sleep_until(last + idle).await;
                if activity.lock().unwrap_or_else(|e| e.into_inner()).elapsed() >= idle {
                    return Err::<(), String>("tunnel idle".into());
                }
            }
        };
        tokio::select! {
            result=async{tokio::try_join!(a,b).map(|_|())}=>result?,
            result=watchdog=>result?,
        }
    } else {
        let mut body = vec![0; length];
        tokio::time::timeout(idle, stream.read_exact(&mut body))
            .await
            .map_err(|_| "body idle timeout")?
            .map_err(|_| "incomplete body")?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(idle)
            .resolve_to_addrs(&target.host, &addresses)
            .gzip(false)
            .build()
            .map_err(|_| "HTTP client unavailable")?;
        let mut request = client
            .request(
                reqwest::Method::from_bytes(method.as_bytes()).map_err(|_| "invalid method")?,
                url.ok_or("missing URL")?,
            )
            .body(body);
        for (key, value) in headers {
            request = request.header(key, value);
        }
        let mut response = request.send().await.map_err(|_| "upstream HTTP failed")?;
        record(audit, Some(&target), "allow", "pinned HTTP destination; redirects are not followed by proxy");
        let status = response.status().as_u16();
        // Header count/bytes remain bounded even when the upstream is hostile.
        let mut output =
            format!("HTTP/1.1 {status} Upstream\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n");
        let connection =
            response.headers().get("connection").and_then(|v| v.to_str().ok()).unwrap_or_default();
        for (key, value) in response.headers().iter() {
            if [
                "connection",
                "content-length",
                "transfer-encoding",
                "keep-alive",
                "upgrade",
                "proxy-authenticate",
                "trailer",
                "te",
            ]
            .contains(&key.as_str())
                || connection.split(',').any(|v| v.trim().eq_ignore_ascii_case(key.as_str()))
            {
                continue;
            }
            let value = value.to_str().map_err(|_| "invalid upstream header")?;
            if output.len() + key.as_str().len() + value.len() + 4 > 8192 {
                return Err("upstream header bound".into());
            }
            output.push_str(key.as_str());
            output.push_str(": ");
            output.push_str(value);
            output.push_str("\r\n");
        }
        output.push_str("\r\n");
        tokio::time::timeout(idle, stream.write_all(output.as_bytes()))
            .await
            .map_err(|_| "write idle")?
            .map_err(|_| "write failed")?;
        let mut bytes = 0u64;
        while let Some(chunk) = response.chunk().await.map_err(|_| "upstream body failed")? {
            bytes = bytes.saturating_add(chunk.len() as u64);
            if bytes > settings.max_transfer_bytes {
                record(audit, Some(&target), "stop", "response transfer bound");
                return Err("transfer bound".into());
            }
            let prefix = format!("{:x}\r\n", chunk.len());
            tokio::time::timeout(idle, async {
                stream.write_all(prefix.as_bytes()).await?;
                stream.write_all(&chunk).await?;
                stream.write_all(b"\r\n").await
            })
            .await
            .map_err(|_| "write idle")?
            .map_err(|_| "write failed")?;
        }
        tokio::time::timeout(idle, stream.write_all(b"0\r\n\r\n"))
            .await
            .map_err(|_| "write idle")?
            .map_err(|_| "write failed")?;
    }
    Ok(())
}
async fn pump(
    reader: &mut (impl tokio::io::AsyncRead + Unpin),
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    idle: Duration,
    limit: u64,
    activity: &Mutex<tokio::time::Instant>,
) -> Result<(), String> {
    let mut buffer = [0; 8192];
    let mut total = 0u64;
    loop {
        let n = reader.read(&mut buffer).await.map_err(|_| "tunnel read")?;
        if n == 0 {
            let _ = writer.shutdown().await;
            return Ok(());
        }
        *activity.lock().unwrap_or_else(|e| e.into_inner()) = tokio::time::Instant::now();
        total = total.saturating_add(n as u64);
        if total > limit {
            return Err("tunnel transfer bound".into());
        }
        tokio::time::timeout(idle, writer.write_all(&buffer[..n]))
            .await
            .map_err(|_| "tunnel write idle")?
            .map_err(|_| "tunnel write")?;
    }
}

/// Tools with local effects only, plus the single contained command launcher.
/// New or external tools are refused until their egress path is accounted for.
pub fn local_tool(name: &str) -> bool {
    [
        "read_file",
        "write_file",
        "edit_file",
        "delete_file",
        "move_file",
        "list_dir",
        "search",
        "read_pdf",
        "run_command",
        "job",
        "ask",
    ]
    .contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_ports_and_label_wildcards_never_grant_suffix_lookalikes() {
        let s = Settings {
            allow: vec!["*.example.org:443".into(), "127.0.0.1:8080".into()],
            ..Default::default()
        };
        assert!(s.validate().is_ok());
        for host in ["a.example.org:443", "deep.a.example.org:443", "127.0.0.1:8080"] {
            assert!(s.permits(&authority(host).unwrap()), "{host}")
        }
        for host in [
            "example.org:443",
            "badexample.org:443",
            "a.example.org:80",
            "example.org.evil.org:443",
            "127.0.0.1:8081",
        ] {
            assert!(!s.permits(&authority(host).unwrap()), "{host}")
        }
        for invalid in ["host", "user@host:443", "host/path:443", "host:0", "host:65536", "host\n:443"] {
            assert!(authority(invalid).is_err(), "{invalid:?}")
        }
    }
    #[test]
    fn dns_special_addresses_and_ipv4_mapped_addresses_do_not_bypass_private_checks() {
        for ip in [
            "127.0.0.1",
            "192.168.1.72",
            "100.64.0.1",
            "198.18.0.1",
            "0.1.2.3",
            "::1",
            "::ffff:127.0.0.1",
            "2001:db8::1",
            "2002:7f00:1::",
        ] {
            assert!(!public(ip.parse().unwrap()), "{ip}")
        }
        for ip in ["1.1.1.1", "8.8.8.8", "2606:4700::1111"] {
            assert!(public(ip.parse().unwrap()), "{ip}")
        }
    }
    #[tokio::test]
    async fn local_dns_requires_explicit_admission_and_audit_ring_reports_reached_omissions() {
        let target = authority("localhost:8080").unwrap();
        assert!(addresses(&target, &Settings::default()).await.is_err());
        assert!(addresses(&target, &Settings { allow_private: true, ..Default::default() }).await.is_ok());
        let mut audit = Audit::default();
        for _ in 0..40 {
            audit.record(Some(&target), "deny", "fixture");
        }
        assert_eq!(audit.decisions.len(), 32);
        assert_eq!(audit.omitted, 8);
    }
}

#[cfg(test)]
mod broker_tests {
    use super::*;
    async fn request(proxy: &Proxy, request: &[u8]) -> String {
        let mut stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, proxy.port)).await.unwrap();
        stream.write_all(request).await.unwrap();
        let mut response = Vec::new();
        let read =
            tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut response)).await.unwrap();
        assert!(read.is_ok() || read.is_err_and(|e| e.kind() == std::io::ErrorKind::ConnectionReset));
        String::from_utf8_lossy(&response).into()
    }
    #[tokio::test]
    async fn a_reached_tunnel_transfer_ceiling_stops_forwarding_before_overspending_it() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let proxy = Proxy::start(&Settings {
            enabled: true,
            allow: vec![format!("127.0.0.1:{port}")],
            max_transfer_bytes: 1024,
            ..Default::default()
        })
        .unwrap();
        let mut stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, proxy.port)).await.unwrap();
        stream.write_all(format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n").as_bytes()).await.unwrap();
        let (mut upstream, _) = listener.accept().await.unwrap();
        let mut header = [0; 39];
        stream.read_exact(&mut header).await.unwrap();
        let payload = vec![b'x'; 2048];
        assert!(payload.len() > 1024);
        stream.write_all(&payload).await.unwrap();
        let mut forwarded = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), upstream.read_to_end(&mut forwarded))
            .await
            .unwrap()
            .unwrap();
        assert!(forwarded.len() <= 1024, "forwarded {} bytes", forwarded.len());
        assert!(
            proxy.report()["decisions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["reason"] == "tunnel transfer bound")
        );
    }
    #[tokio::test]
    async fn continuous_download_outlives_idle_patience_without_client_uploads() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let proxy = Proxy::start(&Settings {
            enabled: true,
            allow: vec![format!("127.0.0.1:{port}")],
            idle_timeout_secs: 1,
            ..Default::default()
        })
        .unwrap();
        let mut stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, proxy.port)).await.unwrap();
        stream.write_all(format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n").as_bytes()).await.unwrap();
        let (mut upstream, _) = listener.accept().await.unwrap();
        let mut header = [0; 39];
        stream.read_exact(&mut header).await.unwrap();
        let producer = tokio::spawn(async move {
            for _ in 0..6 {
                upstream.write_all(b"piece").await.unwrap();
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
        });
        let began = tokio::time::Instant::now();
        for _ in 0..6 {
            let mut piece = [0; 5];
            tokio::time::timeout(Duration::from_secs(10), stream.read_exact(&mut piece))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(&piece, b"piece");
        }
        assert!(began.elapsed() > Duration::from_secs(1));
        producer.await.unwrap();
        drop(stream);
    }
    #[tokio::test]
    async fn malformed_and_over_budget_requests_never_reach_the_allowed_upstream() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let proxy = Proxy::start(&Settings {
            enabled: true,
            allow: vec![format!("127.0.0.1:{port}")],
            ..Default::default()
        })
        .unwrap();
        for headers in [
            "Content-Length: 1\r\nContent-Length: 1\r\n",
            "Transfer-Encoding: chunked\r\n",
            "Content-Length: 1048577\r\n",
        ] {
            let value =
                format!("POST http://127.0.0.1:{port}/private HTTP/1.1\r\nHost: elsewhere\r\n{headers}\r\n");
            assert!(request(&proxy, value.as_bytes()).await.contains("403 Forbidden"));
        }
        let oversized = format!("GET http://127.0.0.1:{port}/ HTTP/1.1\r\nX-Long: {}", "x".repeat(8192));
        let response = request(&proxy, oversized.as_bytes()).await;
        // Rejecting before consuming the oversized header can reset the peer
        // on Windows. The audit below proves the header bound caused refusal.
        assert!(response.is_empty() || response.contains("403 Forbidden"), "{response:?}");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), listener.accept()).await.is_err(),
            "no rejected request connected upstream"
        );
        let audit = proxy.report();
        assert!(audit["decisions"].as_array().unwrap().iter().any(|d| d["reason"] == "header bound"));
    }
    #[tokio::test]
    async fn a_live_tunnel_reaches_the_connection_cap_and_dropping_its_owner_closes_it() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let proxy = Proxy::start(&Settings {
            enabled: true,
            allow: vec![format!("127.0.0.1:{port}")],
            max_connections: 1,
            ..Default::default()
        })
        .unwrap();
        let mut stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, proxy.port)).await.unwrap();
        stream
            .write_all(
                format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes(),
            )
            .await
            .unwrap();
        let (mut upstream, _) = listener.accept().await.unwrap();
        let mut header = [0; 39];
        stream.read_exact(&mut header).await.unwrap();
        assert!(String::from_utf8_lossy(&header).contains("200 Connection Established"));
        let refused = request(&proxy, b"CONNECT denied.example:443 HTTP/1.1\r\n\r\n").await;
        assert!(refused.is_empty() || refused.contains("503 Service Unavailable"));
        assert!(
            proxy.report()["decisions"].as_array().unwrap().iter().any(|d| d["reason"] == "connection bound")
        );
        let port = proxy.port;
        let retired = proxy.task.abort_handle();
        drop(proxy);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(10), upstream.read_u8())
                .await
                .unwrap()
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        // A child tunnel can close on another worker while the owner's future
        // is still dropping its listener. Wait for that destructor to finish.
        tokio::time::timeout(Duration::from_secs(10), async {
            while !retired.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the retired proxy must release its listener");
        assert!(TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).await.is_err());
    }
}
