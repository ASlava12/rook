//! Where a request goes on its way out.
//!
//! Some APIs are reachable only through a proxy, and on the same machine others
//! must not go near one. So this is written down per thing that reaches the
//! network rather than set once for the process: an endpoint, the web tools, an
//! MCP server, a language-server download. Every one of them ends up here.
//!
//! Lives in this crate because it is the lowest one that speaks HTTP, and the
//! other three that do — `rook-tools`, `rook-mcp`, `rook-core` — all sit above
//! it. One answer to "does this request take a proxy" rather than four.

/// This machine and this network, in the spelling reqwest reads.
///
/// The same set [`crate::beside_us`] answers for, said twice because the two
/// are asked at different moments: `beside_us` about one known address before a
/// client is built, this about every request a client will ever make. A
/// `web_fetch` client is built once and then asked for pages nobody has named
/// yet, so a bypass decided at build time would send a page on this network
/// through the proxy. `a_proxy_never_takes_a_request_to_this_network` holds the
/// two together.
const BESIDE_US: &str = "localhost,127.0.0.0/8,::1,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16,\
                         169.254.0.0/16,fc00::/7,fe80::/10,.local,.localhost";

/// What to do with a request that is about to leave this machine.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Proxy {
    /// However the machine is set up — `http_proxy`, `HTTPS_PROXY`, `no_proxy`.
    ///
    /// The default, because it is what every build did before any of this
    /// existed: a configuration that says nothing about proxies must keep
    /// behaving exactly as it did.
    #[default]
    AsTheEnvironmentSays,
    /// Straight out, whatever the environment says.
    ///
    /// Worth spelling: a machine with `http_proxy` set for the shell is one
    /// where a single endpoint may still have to be reached directly, and
    /// unsetting the variable is not an option when everything else needs it.
    Direct,
    /// Through this one. `http://`, `https://`, `socks5://` or `socks5h://`,
    /// with credentials in the URL where the proxy wants them.
    Through(String),
}

impl Proxy {
    /// Read a configured value. Empty inherits, so a field nobody set does not
    /// mean "no proxy" — it means "nothing said here".
    pub fn parse(written: &str) -> Self {
        match written.trim() {
            "" => Self::AsTheEnvironmentSays,
            // Three spellings because people reach for different ones, and one
            // that was meant to turn proxying off and instead became a hostname
            // would be every request sent somewhere nobody chose.
            "direct" | "none" | "off" => Self::Direct,
            url => Self::Through(url.to_string()),
        }
    }

    /// What this part says, or what was said for everything if this part said
    /// nothing.
    pub fn or(self, wider: Self) -> Self {
        match self {
            Self::AsTheEnvironmentSays => wider,
            said => said,
        }
    }

    /// Put it on a client that is about to be built.
    ///
    /// `base` is the one address this client will ever talk to, where there is
    /// one — a model endpoint, an MCP server — and `None` for a client that
    /// will be asked for addresses nobody has named yet.
    ///
    /// Either way a request to this machine or this network goes straight out.
    /// That rule is older than this module: sent through a VPN, a request to
    /// the desk next door comes back as whatever the tunnel makes of an address
    /// it cannot route to. A per-endpoint proxy must not be a way to
    /// reintroduce it by accident, so the exception rides on the proxy itself
    /// and is applied per request rather than decided once here. Somebody whose
    /// proxy really is the way into their own network reaches it by a name, and
    /// a name is not one of these.
    pub fn on(
        &self,
        builder: reqwest::ClientBuilder,
        base: Option<&str>,
    ) -> std::result::Result<reqwest::ClientBuilder, String> {
        match self {
            // The machine's own `no_proxy` may not list this network, and
            // before any of this existed that was answered by turning proxying
            // off for such a client outright. Kept, because it is what a
            // configuration saying nothing about proxies has always done.
            Self::AsTheEnvironmentSays => match base.is_some_and(crate::beside_us) {
                true => Ok(builder.no_proxy()),
                false => Ok(builder),
            },
            Self::Direct => Ok(builder.no_proxy()),
            Self::Through(url) => match reqwest::Proxy::all(url.as_str()) {
                Ok(proxy) => Ok(builder.proxy(proxy.no_proxy(reqwest::NoProxy::from_string(BESIDE_US)))),
                // Named rather than quietly ignored: a proxy that does not
                // build is a request about to go straight out to somewhere it
                // was configured not to reach directly, which is the opposite
                // of what was asked for.
                Err(e) => Err(format!(
                    "{url:?} is not a proxy this can use ({e}). It has to be http://, https://, \
                     socks5:// or socks5h://, with a host and a port."
                )),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BESIDE_US, Proxy};

    #[test]
    fn a_field_nobody_set_inherits_rather_than_turning_proxying_off() {
        assert_eq!(Proxy::parse(""), Proxy::AsTheEnvironmentSays);
        assert_eq!(Proxy::parse("   "), Proxy::AsTheEnvironmentSays);
        let wider = Proxy::Through("socks5://127.0.0.1:1080".into());
        assert_eq!(Proxy::parse("").or(wider.clone()), wider, "and inheriting is what it does");
    }

    /// A machine with `http_proxy` set for the shell is one where a single
    /// endpoint may still have to be reached directly.
    #[test]
    fn a_part_can_say_direct_against_a_proxy_set_for_everything() {
        let wider = Proxy::Through("http://gateway:3128".into());
        for spelling in ["direct", "none", "off"] {
            assert_eq!(Proxy::parse(spelling).or(wider.clone()), Proxy::Direct, "{spelling}");
        }
    }

    #[test]
    fn a_part_with_a_proxy_of_its_own_keeps_it() {
        let mine = Proxy::parse("socks5://127.0.0.1:9050");
        assert_eq!(mine.clone().or(Proxy::Through("http://gateway:3128".into())), mine);
    }

    /// The rule that predates this module, and the one thing a per-part proxy
    /// must not be able to undo. Asserted against the list the proxy itself
    /// carries, because a client asked for pages nobody has named yet cannot
    /// have it decided when it is built.
    #[test]
    fn a_proxy_never_takes_a_request_to_this_network() {
        let listed = |host: &str| {
            BESIDE_US.split(',').any(|pattern| match pattern.strip_prefix('.') {
                Some(suffix) => host.ends_with(suffix),
                None => pattern == host || covers(pattern, host),
            })
        };
        for (url, host) in [
            ("http://192.168.1.100:8080/v1", "192.168.1.100"),
            ("http://10.1.2.3/v1", "10.1.2.3"),
            ("http://172.16.5.4/v1", "172.16.5.4"),
            ("http://127.0.0.1:1234/v1", "127.0.0.1"),
            ("http://localhost:11434", "localhost"),
            ("http://box.local:8080", "box.local"),
        ] {
            // The precondition, and it is the point: these are exactly the
            // addresses the older rule already refused to proxy. Without it
            // this test would pass on a list that had quietly lost one.
            assert!(crate::beside_us(url), "{url} is beside us by the older rule");
            assert!(listed(host), "so {host} has to be in the proxy's own bypass: {BESIDE_US}");
        }
        // And an address that is not beside us is not in it, or the bypass
        // would be turning the proxy off for everything.
        assert!(!listed("api.example.com"), "a public host is not bypassed");
        assert!(!listed("8.8.8.8"), "nor a public address");
    }

    /// Whether a CIDR pattern covers a literal address, for the test above.
    fn covers(pattern: &str, host: &str) -> bool {
        let Some((network, bits)) = pattern.split_once('/') else { return false };
        let (Ok(network), Ok(host), Ok(bits)) =
            (network.parse::<std::net::IpAddr>(), host.parse::<std::net::IpAddr>(), bits.parse::<u32>())
        else {
            return false;
        };
        match (network, host) {
            (std::net::IpAddr::V4(n), std::net::IpAddr::V4(h)) => {
                let mask = u32::MAX.checked_shl(32 - bits).unwrap_or(0);
                u32::from(n) & mask == u32::from(h) & mask
            }
            _ => false,
        }
    }

    /// What the older rule did for a client with one fixed address on this
    /// network, and still does: proxying off outright, because the machine's
    /// own `no_proxy` may not list it.
    #[test]
    fn a_client_for_one_address_beside_us_takes_no_proxy_from_the_environment() {
        // Building a client needs the crypto provider this crate installs, the
        // same one every provider installs before its own client.
        crate::init_tls();
        let built = Proxy::AsTheEnvironmentSays
            .on(reqwest::Client::builder(), Some("http://192.168.1.100:8080/v1"))
            .expect("it builds");
        assert!(built.build().is_ok());
    }

    #[test]
    fn a_proxy_that_is_not_a_url_says_so_rather_than_going_out_directly() {
        let why = Proxy::Through("not a proxy".into())
            .on(reqwest::Client::builder(), Some("https://api.example.com"))
            .expect_err("it cannot be used");
        assert!(why.contains("not a proxy"), "it quotes what was written: {why}");
        assert!(why.contains("socks5"), "and says what would work: {why}");
    }

    #[test]
    fn every_scheme_a_proxy_is_written_in_builds() {
        crate::init_tls();
        for url in [
            "http://gateway:3128",
            "https://gateway:3129",
            "socks5://127.0.0.1:1080",
            "socks5h://127.0.0.1:1080",
        ] {
            let built = Proxy::Through(url.into())
                .on(reqwest::Client::builder(), Some("https://api.example.com"))
                .unwrap_or_else(|why| panic!("{url} has to be usable: {why}"));
            assert!(built.build().is_ok(), "{url} builds a client");
        }
    }
}
