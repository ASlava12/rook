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

/// Where a server comes from, and how it is checked.
pub enum Source {
    /// A GitHub release listing a digest per asset, shipping one gzipped
    /// binary per unix target. Checked here, byte for byte, as it arrives.
    GithubGz { repo: &'static str },
    /// Packages from the npm registry, installed under a prefix of ours with
    /// their install scripts not run. npm checks each tarball against the
    /// integrity hash the registry lists; the binary is a shim under
    /// `node_modules/.bin`.
    Npm { packages: &'static [&'static str] },
    /// Built from source by the Go toolchain, which fetches the module through
    /// the proxy and checks it against the checksum database.
    Go { module: &'static str },
}

pub struct Recipe {
    pub command: &'static str,
    pub source: Source,
}

pub const RUST_ANALYZER: Recipe =
    Recipe { command: "rust-analyzer", source: Source::GithubGz { repo: "rust-lang/rust-analyzer" } };
pub const TYPESCRIPT: Recipe = Recipe {
    command: "typescript-language-server",
    source: Source::Npm { packages: &["typescript-language-server", "typescript"] },
};
pub const PYRIGHT: Recipe =
    Recipe { command: "pyright-langserver", source: Source::Npm { packages: &["pyright"] } };
pub const GOPLS: Recipe =
    Recipe { command: "gopls", source: Source::Go { module: "golang.org/x/tools/gopls" } };

/// The recipe for a name somebody typed — the server's or the language's.
pub fn recipe_for(name: &str) -> Option<&'static Recipe> {
    match name {
        "rust-analyzer" | "rust" => Some(&RUST_ANALYZER),
        "typescript-language-server" | "typescript" | "javascript" => Some(&TYPESCRIPT),
        "pyright-langserver" | "pyright" | "python" => Some(&PYRIGHT),
        "gopls" | "go" => Some(&GOPLS),
        _ => None,
    }
}

impl Recipe {
    /// How the machine's own tooling would install this: what a person at the
    /// keyboard would type. Runs as a command through the policy like any
    /// other, and only a `free` stance reaches for it.
    pub fn system_command(&self) -> Option<String> {
        match &self.source {
            Source::GithubGz { .. } if self.command == "rust-analyzer" => {
                Some("rustup component add rust-analyzer".into())
            }
            Source::GithubGz { .. } => None,
            Source::Npm { packages } => Some(format!("npm install -g {}", packages.join(" "))),
            Source::Go { module } => Some(format!("go install {module}@latest")),
        }
    }

    /// The command that installs this under `dir` and nowhere else, for the
    /// sources that are a command rather than a download. `None` is a source
    /// this fetches itself.
    pub fn command_into(&self, dir: &Path) -> Option<(String, Vec<(String, String)>)> {
        match &self.source {
            Source::GithubGz { .. } => None,
            // Scripts off: a package's `postinstall` is arbitrary code, and a
            // language server has no business running any at install time.
            Source::Npm { packages } => Some((
                format!(
                    "npm install --ignore-scripts --no-audit --no-fund --prefix \"{}\" {}",
                    dir.display(),
                    packages.iter().map(|p| format!("{p}@latest")).collect::<Vec<_>>().join(" ")
                ),
                Vec::new(),
            )),
            Source::Go { module } => Some((
                format!("go install {module}@latest"),
                vec![("GOBIN".into(), dir.display().to_string())],
            )),
        }
    }

    /// The toolchain a command-shaped source needs, by the name the
    /// environment probes use. `None` needs nothing but a network.
    pub fn needs(&self) -> Option<&'static str> {
        match &self.source {
            Source::GithubGz { .. } => None,
            Source::Npm { .. } => Some("npm"),
            Source::Go { .. } => Some("go"),
        }
    }

    /// Where the binary is inside a `current` directory.
    pub fn binary_in(&self, current: &Path) -> PathBuf {
        let path = match &self.source {
            Source::Npm { .. } => current.join("node_modules").join(".bin").join(self.command),
            _ => current.join(self.command),
        };
        match (cfg!(windows), &self.source) {
            (true, Source::Npm { .. }) => path.with_extension("cmd"),
            (true, _) => path.with_extension("exe"),
            _ => path,
        }
    }

    /// In words, what an install of this checked and what it did not.
    fn checked(&self, tag: &str, asset: &str, sha256: &str) -> (String, String) {
        match &self.source {
            Source::GithubGz { repo } => (
                format!("sha256 {sha256} matches the digest {repo} listed for {asset} in release {tag}"),
                "the release itself was not reviewed — the digest and the file come from the same \
                 publisher, so this shows the download is intact, not that the program is good"
                    .into(),
            ),
            Source::Npm { .. } => (
                "npm checked each tarball against the integrity hash the registry lists, and \
                 ran no install scripts"
                    .into(),
                "the packages themselves were not reviewed, and neither were their dependencies — \
                 the registry's hash says what was published is what arrived, not that it is good"
                    .into(),
            ),
            Source::Go { .. } => (
                "the Go toolchain fetched the module through the proxy and checked it against \
                 the checksum database, then built it from source"
                    .into(),
                "the source was not reviewed — the checksum database says the module is the one \
                 everyone else gets, not that it is good"
                    .into(),
            ),
        }
    }

    /// The asset this platform can run, or why there is none for it. Only a
    /// GitHub source has assets.
    pub fn asset_for(&self, os: &str, arch: &str, userland: &str) -> Result<String, String> {
        let Source::GithubGz { repo } = &self.source else {
            return Err(format!("{} is not fetched from a release", self.command));
        };
        match os {
            "macos" => Ok(format!("{}-{arch}-apple-darwin.gz", self.command)),
            "linux" => {
                let libc = if userland == "musl" { "musl" } else { "gnu" };
                Ok(format!("{}-{arch}-unknown-linux-{libc}.gz", self.command))
            }
            "windows" => Err(format!(
                "{} is published for Windows as a zip, which this cannot open yet — take it from \
                 https://github.com/{repo}/releases",
                self.command
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
/// there is one — asked of the recipe, which knows where inside `current` it
/// lives.
pub fn current(command: &str) -> PathBuf {
    let current = current_in(&crate::paths::servers_dir(), command);
    match recipe_for(command) {
        Some(recipe) => recipe.binary_in(&current),
        None => current.join(command),
    }
}

/// The `current` directory under any root: an installer told where to put
/// things must put `current` there too, or a test of one writes into the real
/// state directory of whoever runs it.
fn current_in(into: &Path, command: &str) -> PathBuf {
    into.join(command).join("current")
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
        let Source::GithubGz { repo } = &recipe.source else {
            return Err(format!("{} has no release to look up", recipe.command));
        };
        let url = format!("{}/repos/{repo}/releases/latest", self.api);
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
            .ok_or_else(|| format!("release {tag} of {repo} has no asset named {asset}"))?;
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

    /// Fetch or build, check, put in place under `current`.
    pub async fn install(
        &self,
        recipe: &Recipe,
        env: &rook_skills::Environment,
    ) -> Result<Installed, String> {
        if let Some(tool) = recipe.needs()
            && !env.tools.contains_key(tool)
            && !env.languages.contains_key(tool)
        {
            return Err(format!(
                "{} is installed with `{tool}`, which this machine does not have",
                recipe.command
            ));
        }
        let current = current_in(&self.into, recipe.command);
        let make = |dir: &Path| {
            std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))
        };

        let (tag, verified, unverified) = match recipe.command_into(&current) {
            // The prefix has to exist for the tool to install into it.
            Some((command, env_vars)) => {
                make(&current)?;
                run_to_completion(&command, &current, &env_vars).await?;
                let (verified, unverified) = recipe.checked("latest", "", "");
                ("latest".to_string(), verified, unverified)
            }
            None => {
                let asset = recipe.asset_for(&env.os, &env.arch, &env.userland)?;
                let release = self.latest(recipe, &asset).await?;
                let bytes = self.download(&release).await?;
                // Nothing on disk until the bytes have matched: a refused
                // download leaves not even a directory behind.
                let versioned = self.into.join(recipe.command).join(&release.tag);
                make(&versioned)?;
                make(&current)?;
                let binary = versioned.join(recipe.command);
                unpack_gz(&bytes, &binary)?;
                executable(&binary)?;
                // A copy rather than a link: a link needs a privilege on Windows
                // that an ordinary account does not have, and one binary is
                // cheap next to the download that produced it.
                let placed = recipe.binary_in(&current);
                std::fs::copy(&binary, &placed)
                    .map_err(|e| format!("could not put {} in place: {e}", placed.display()))?;
                executable(&placed)?;
                let (verified, unverified) = recipe.checked(&release.tag, &asset, &release.sha256);
                (release.tag, verified, unverified)
            }
        };

        let path = recipe.binary_in(&current);
        if !path.is_file() {
            return Err(format!("{} finished but left no {} behind", recipe.command, path.display()));
        }
        Ok(Installed { command: recipe.command.into(), tag, path, verified, unverified })
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

/// Run an installer's command to the end, through the machine's shell, with
/// both pipes drained and both ends of what they said kept. A failed install
/// explains itself in its last lines, after however much progress it printed,
/// so the tail is what the error carries.
async fn run_to_completion(command: &str, cwd: &Path, env: &[(String, String)]) -> Result<(), String> {
    const KEPT: usize = 64 << 10;
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let mut child = rook_tools::exec::spawn_shell(command, cwd, &env).map_err(|e| e.to_string())?;
    let (mut out, mut err) = (child.stdout.take(), child.stderr.take());
    let printed = std::sync::Mutex::new(rook_tools::jobs::Printed::default());
    let reading = async {
        tokio::join!(keep_ends(&mut out, &printed, KEPT), keep_ends(&mut err, &printed, KEPT));
    };
    let (_, status) = tokio::join!(reading, child.wait());
    let status = status.map_err(|e| format!("`{command}` could not be waited for: {e}"))?;
    match status.success() {
        true => Ok(()),
        false => {
            let said = printed.lock().unwrap_or_else(|e| e.into_inner()).seen();
            Err(format!("`{command}` failed ({status}): {}", said.trim()))
        }
    }
}

/// Drain one pipe into the shared record, so a writer is never blocked on a
/// full pipe and only the memory is bounded.
async fn keep_ends(
    stream: &mut Option<impl tokio::io::AsyncRead + Unpin>,
    into: &std::sync::Mutex<rook_tools::jobs::Printed>,
    cap: usize,
) {
    use tokio::io::AsyncReadExt;
    let Some(stream) = stream else { return };
    let mut chunk = vec![0u8; 16 * 1024];
    while let Ok(n) = stream.read(&mut chunk).await {
        if n == 0 {
            return;
        }
        into.lock().unwrap_or_else(|e| e.into_inner()).push(&String::from_utf8_lossy(&chunk[..n]), cap);
    }
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
    fn each_source_says_how_it_installs_and_what_that_checks() {
        let (cmd, env) = TYPESCRIPT.command_into(Path::new("/srv/ts")).unwrap();
        assert!(cmd.contains("--ignore-scripts"), "no package script runs at install: {cmd}");
        assert!(cmd.contains("--prefix \"/srv/ts\""), "and it goes where it was told: {cmd}");
        assert!(
            cmd.contains("typescript-language-server@latest") && cmd.contains(" typescript@latest"),
            "{cmd}"
        );
        assert!(env.is_empty());
        let (cmd, env) = GOPLS.command_into(Path::new("/srv/go")).unwrap();
        assert_eq!(cmd, "go install golang.org/x/tools/gopls@latest");
        assert_eq!(
            env,
            vec![("GOBIN".to_string(), "/srv/go".to_string())],
            "which is how it lands under ours"
        );
        assert!(RUST_ANALYZER.command_into(Path::new("/srv")).is_none(), "fetched, not run");

        assert_eq!(TYPESCRIPT.needs(), Some("npm"));
        assert_eq!(GOPLS.needs(), Some("go"));
        assert_eq!(RUST_ANALYZER.needs(), None);

        let (verified, unverified) = TYPESCRIPT.checked("", "", "");
        assert!(verified.contains("integrity hash") && verified.contains("no install scripts"), "{verified}");
        assert!(unverified.contains("not reviewed"), "{unverified}");
        let (verified, _) = GOPLS.checked("", "", "");
        assert!(verified.contains("checksum database"), "{verified}");
    }

    /// Spelled for both platforms: on Windows a binary is `.exe` and npm's
    /// shim is `.cmd`, and a test that wrote the unix names read the Windows
    /// runner as putting them in the wrong place.
    #[test]
    fn the_binary_is_where_the_source_puts_it() {
        let current = Path::new("/state/servers/x/current");
        let runs_as = |path: PathBuf, windows_ext: &str| match cfg!(windows) {
            true => path.with_extension(windows_ext),
            false => path,
        };
        assert_eq!(RUST_ANALYZER.binary_in(current), runs_as(current.join("rust-analyzer"), "exe"));
        assert_eq!(GOPLS.binary_in(current), runs_as(current.join("gopls"), "exe"));
        assert_eq!(
            TYPESCRIPT.binary_in(current),
            runs_as(current.join("node_modules").join(".bin").join("typescript-language-server"), "cmd"),
            "npm's shim, under the prefix"
        );
    }

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
