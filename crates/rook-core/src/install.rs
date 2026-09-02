//! Fetching a language server this machine does not have.
//!
//! One publishing shape so far: a GitHub release whose API lists a digest for
//! every asset, and whose asset for this platform is a single gzipped binary.
//! That is rust-analyzer. clangd ships zip and xz, gopls is built from source
//! and the node servers come from npm — each is a shape of its own, and a name
//! this cannot fetch is answered with where to get it rather than an attempt.
//!
//! What was checked and what was not is written into what the person reads:
//! the bytes match the digest the publisher listed for that asset in the same
//! release. That proves the download is intact. Nothing here reviews the
//! release, and the report says so.

use std::path::{Path, PathBuf};

use sha2::Digest;

/// Where a server comes from.
pub struct Recipe {
    pub command: &'static str,
    repo: &'static str,
}

pub const RUST_ANALYZER: Recipe = Recipe { command: "rust-analyzer", repo: "rust-lang/rust-analyzer" };

/// The recipe for a name somebody typed, if there is one.
pub fn recipe_for(name: &str) -> Option<&'static Recipe> {
    match name {
        "rust-analyzer" | "rust" => Some(&RUST_ANALYZER),
        _ => None,
    }
}

impl Recipe {
    /// How the machine's own tooling would install this, when it has any: what
    /// a person at the keyboard would type. Runs as a command through the
    /// policy like any other, and only a `free` stance reaches for it.
    pub fn system_command(&self) -> Option<&'static str> {
        match self.command {
            "rust-analyzer" => Some("rustup component add rust-analyzer"),
            _ => None,
        }
    }

    /// The asset this platform can run, or why there is none for it.
    pub fn asset_for(&self, os: &str, arch: &str, userland: &str) -> Result<String, String> {
        match os {
            "macos" => Ok(format!("{}-{arch}-apple-darwin.gz", self.command)),
            "linux" => {
                let libc = if userland == "musl" { "musl" } else { "gnu" };
                Ok(format!("{}-{arch}-unknown-linux-{libc}.gz", self.command))
            }
            "windows" => Err(format!(
                "{} is published for Windows as a zip, which this cannot open yet — take it from \
                 https://github.com/{}/releases",
                self.command, self.repo
            )),
            other => Err(format!("{} publishes no build for {other}", self.command)),
        }
    }
}

/// One asset of the latest release, as its API describes it.
#[derive(Debug)]
pub struct Asset {
    pub tag: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

/// A server fetched, checked, unpacked and put in place — and what was and was
/// not checked on the way, in words.
#[derive(Debug)]
pub struct Installed {
    pub command: String,
    pub tag: String,
    pub path: PathBuf,
    pub verified: String,
    pub unverified: String,
}

/// An asset past this is not a language server, whatever it is called.
const MOST_ASSET_BYTES: usize = 256 << 20;
/// A gzip that inflates past this is a bomb, not a binary.
const MOST_UNPACKED_BYTES: u64 = 512 << 20;
/// The API answer is a few kilobytes; a megabyte of it is a different server.
const MOST_API_BYTES: usize = 4 << 20;
const MOST_HOPS: usize = 4;

/// The binary `rook lsp install` put in place for `command`, whether or not
/// there is one.
pub fn current(command: &str) -> PathBuf {
    current_in(&crate::paths::servers_dir(), command)
}

/// The same under any directory: an installer told where to put things must
/// put `current` there too, or a test of one writes into the real state
/// directory of whoever runs it.
fn current_in(into: &Path, command: &str) -> PathBuf {
    let path = into.join(command).join("current").join(command);
    if cfg!(windows) { path.with_extension("exe") } else { path }
}

pub struct Installer {
    client: reqwest::Client,
    api: String,
    into: PathBuf,
}

impl Installer {
    /// Against GitHub, into `into`.
    ///
    /// `ROOK_RELEASE_API` overrides where GitHub is, which is a seam and says
    /// so: a test of the agent deciding to install must not reach GitHub, and
    /// the loop builds this itself rather than taking it as an argument.
    pub fn new(into: PathBuf) -> Result<Self, String> {
        let api = std::env::var("ROOK_RELEASE_API").unwrap_or_else(|_| "https://api.github.com".into());
        Self::at(api, into)
    }

    /// Against whatever answers like GitHub's API at `api`, which is how a
    /// test stands in for it: the alternative is a test that reaches GitHub.
    #[doc(hidden)]
    pub fn at(api: String, into: PathBuf) -> Result<Self, String> {
        // The same provider the model client installs, for the same reason:
        // the rustls default needs a C toolchain, which is the FreeBSD blocker.
        rook_llm::init_tls();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(600))
            .connect_timeout(std::time::Duration::from_secs(15))
            // Followed by hand, and only to hosts a release download is known
            // to go through: an approval named an address, and a redirect
            // elsewhere is how that becomes a request nobody agreed to.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("rook/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| format!("could not build an HTTP client: {e}"))?;
        Ok(Self { client, api, into })
    }

    /// The latest release's asset named `asset`, with the digest the publisher
    /// listed for it.
    pub async fn latest(&self, recipe: &Recipe, asset: &str) -> Result<Asset, String> {
        let url = format!("{}/repos/{}/releases/latest", self.api, recipe.repo);
        let response = self
            .client
            .get(&url)
            .header("accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| format!("could not reach {url}: {e}"))?;
        if !response.status().is_success() {
            return Err(format!("{url} answered {}", response.status()));
        }
        let (body, _) = read_bounded(response, MOST_API_BYTES).await?;
        let release: serde_json::Value =
            serde_json::from_slice(&body).map_err(|e| format!("{url} did not answer with a release: {e}"))?;
        let tag = release["tag_name"].as_str().unwrap_or("").to_string();
        let found = release["assets"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|a| a["name"].as_str() == Some(asset))
            .ok_or_else(|| format!("release {tag} of {} has no asset named {asset}", recipe.repo))?;
        let Some(sha256) = listed_sha256(found) else {
            return Err(format!(
                "release {tag} lists no sha256 digest for {asset}, so a download could not be checked \
                 — nothing was fetched"
            ));
        };
        Ok(Asset {
            tag,
            url: found["browser_download_url"].as_str().unwrap_or("").to_string(),
            sha256,
            size: found["size"].as_u64().unwrap_or(0),
        })
    }

    /// Fetch, check, unpack, put in place.
    pub async fn install(
        &self,
        recipe: &Recipe,
        env: &rook_skills::Environment,
    ) -> Result<Installed, String> {
        let asset = recipe.asset_for(&env.os, &env.arch, &env.userland)?;
        let release = self.latest(recipe, &asset).await?;
        let bytes = self.download(&release).await?;

        let versioned = self.into.join(recipe.command).join(&release.tag);
        std::fs::create_dir_all(&versioned)
            .map_err(|e| format!("could not create {}: {e}", versioned.display()))?;
        let binary = versioned.join(recipe.command);
        unpack_gz(&bytes, &binary)?;
        executable(&binary)?;

        // `current` is a copy rather than a link: a link needs a privilege on
        // Windows that an ordinary account does not have, and a copy of one
        // binary is cheap next to the download that produced it.
        let current = current_in(&self.into, recipe.command);
        if let Some(dir) = current.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        }
        std::fs::copy(&binary, &current)
            .map_err(|e| format!("could not put {} in place: {e}", current.display()))?;
        executable(&current)?;

        Ok(Installed {
            command: recipe.command.into(),
            tag: release.tag.clone(),
            path: current,
            verified: format!(
                "sha256 {} matches the digest {} listed for {asset} in release {}",
                release.sha256, recipe.repo, release.tag
            ),
            unverified: "the release itself was not reviewed — the digest and the file come from \
                         the same publisher, so this shows the download is intact, not that the \
                         program is good"
                .into(),
        })
    }

    /// The asset's bytes, checked against the digest as they arrive. A body
    /// past the cap is refused while it is still coming rather than measured
    /// once it is all here.
    async fn download(&self, release: &Asset) -> Result<Vec<u8>, String> {
        let mut at = release.url.clone();
        let origin = host_of(&at);
        let mut landed = None;
        for _ in 0..MOST_HOPS {
            let hop = self.client.get(&at).send().await.map_err(|e| format!("could not fetch {at}: {e}"))?;
            let Some(to) = hop.headers().get(reqwest::header::LOCATION).and_then(|v| v.to_str().ok()) else {
                landed = Some(hop);
                break;
            };
            let to = to.to_string();
            let host = host_of(&to);
            // Where GitHub keeps release files, and nowhere else.
            let known = host == origin
                || host == "objects.githubusercontent.com"
                || host == "release-assets.githubusercontent.com";
            if !known {
                return Err(format!("{at} redirects to {to}, which is not where a release is kept"));
            }
            at = to;
        }
        let Some(response) = landed else {
            return Err(format!("{} redirected more than {MOST_HOPS} times", release.url));
        };
        if !response.status().is_success() {
            return Err(format!("{at} answered {}", response.status()));
        }
        let (bytes, sha256) = read_bounded(response, MOST_ASSET_BYTES).await?;
        if sha256 != release.sha256 {
            return Err(format!(
                "the download does not match the digest the release lists: got sha256 {sha256}, \
                 the release says {} — nothing was installed",
                release.sha256
            ));
        }
        if release.size != 0 && bytes.len() as u64 != release.size {
            return Err(format!(
                "the download is {} bytes and the release says {} — nothing was installed",
                bytes.len(),
                release.size
            ));
        }
        Ok(bytes)
    }
}

/// The digest an asset entry lists, if it lists one that could check anything.
///
/// `sha256:` followed by nothing, or by half a hash, is not a digest — and a
/// download compared against an empty one would fail to match rather than be
/// refused for having nothing to match, which is the wrong message.
fn listed_sha256(asset: &serde_json::Value) -> Option<String> {
    let hex = asset["digest"].as_str()?.strip_prefix("sha256:")?.to_ascii_lowercase();
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then_some(hex)
}

/// The body and its sha256, read together so the hash is of what arrived and
/// the cap is applied before the bytes are kept.
async fn read_bounded(mut response: reqwest::Response, most: usize) -> Result<(Vec<u8>, String), String> {
    let mut body = Vec::new();
    let mut hasher = sha2::Sha256::new();
    loop {
        match response.chunk().await {
            Ok(None) => return Ok((body, hex::encode(hasher.finalize()))),
            Ok(Some(chunk)) => {
                hasher.update(&chunk);
                body.extend_from_slice(&chunk);
                if body.len() > most {
                    return Err(format!("more than {most} bytes arrived and it was still coming — refused"));
                }
            }
            Err(e) => return Err(format!("the download stopped partway: {e}")),
        }
    }
}

/// Inflate one gzipped file to `to`, stopping past the cap: what a gzip
/// inflates to is decided by whoever made it.
fn unpack_gz(bytes: &[u8], to: &Path) -> Result<(), String> {
    use std::io::{Read, Write};
    let mut reader = flate2::read::GzDecoder::new(bytes);
    let mut out = std::fs::File::create(to).map_err(|e| format!("could not write {}: {e}", to.display()))?;
    let mut chunk = vec![0u8; 64 << 10];
    let mut written: u64 = 0;
    loop {
        let n = reader
            .read(&mut chunk)
            .map_err(|e| format!("{} is not a gzip this can open: {e}", to.display()))?;
        if n == 0 {
            return Ok(());
        }
        written += n as u64;
        if written > MOST_UNPACKED_BYTES {
            drop(out);
            let _ = std::fs::remove_file(to);
            return Err(format!("the archive inflates past {MOST_UNPACKED_BYTES} bytes — refused"));
        }
        out.write_all(&chunk[..n]).map_err(|e| format!("could not write {}: {e}", to.display()))?;
    }
}

fn executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("could not make {} executable: {e}", path.display()))?;
    }
    let _ = path;
    Ok(())
}

fn host_of(url: &str) -> String {
    url.split("://").nth(1).unwrap_or(url).split('/').next().unwrap_or("").to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_asset_follows_the_platform_and_says_when_there_is_none() {
        let ra = &RUST_ANALYZER;
        assert_eq!(ra.asset_for("macos", "aarch64", "bsd").unwrap(), "rust-analyzer-aarch64-apple-darwin.gz");
        assert_eq!(
            ra.asset_for("linux", "x86_64", "gnu").unwrap(),
            "rust-analyzer-x86_64-unknown-linux-gnu.gz"
        );
        assert_eq!(
            ra.asset_for("linux", "aarch64", "musl").unwrap(),
            "rust-analyzer-aarch64-unknown-linux-musl.gz"
        );
        let windows = ra.asset_for("windows", "x86_64", "msvc").unwrap_err();
        assert!(windows.contains("zip") && windows.contains("releases"), "{windows}");
        assert!(ra.asset_for("freebsd", "x86_64", "bsd").unwrap_err().contains("no build"));
    }

    #[test]
    fn a_digest_is_sixty_four_hex_digits_or_it_is_not_one() {
        let listed = |d: &str| listed_sha256(&serde_json::json!({ "digest": d }));
        assert_eq!(listed(&format!("sha256:{}", "ab".repeat(32))).as_deref(), Some("ab".repeat(32).as_str()));
        assert_eq!(listed("sha256:"), None, "empty is not a digest");
        assert_eq!(listed("sha256:abc"), None, "nor is a fragment");
        assert_eq!(listed("md5:abcdef"), None, "nor another algorithm");
        assert_eq!(listed_sha256(&serde_json::json!({})), None);
    }

    #[test]
    fn a_gzip_that_inflates_without_end_is_refused_while_inflating() {
        use std::io::Write;
        // A few hundred bytes of gzip that inflate to a gigabyte.
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        let zeros = vec![0u8; 1 << 20];
        for _ in 0..1024 {
            encoder.write_all(&zeros).unwrap();
        }
        let bomb = encoder.finish().unwrap();
        assert!(bomb.len() < 4 << 20, "the point is that it is small: {} bytes", bomb.len());

        let dir = tempfile::tempdir().unwrap();
        let to = dir.path().join("out");
        let refused = unpack_gz(&bomb, &to).unwrap_err();
        assert!(refused.contains("inflates past"), "{refused}");
        assert!(!to.exists(), "and nothing half-written is left behind");
    }
}
