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
    /// A GitHub release listing a digest per asset. The asset is one gzipped
    /// binary or a zip; both are checked here, byte for byte, as they arrive.
    /// `strip_top` drops the archive's single top-level directory, which is
    /// how clangd ships (`clangd_<version>/bin/clangd`).
    Github { repo: &'static str, strip_top: bool },
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

pub const RUST_ANALYZER: Recipe = Recipe {
    command: "rust-analyzer",
    source: Source::Github { repo: "rust-lang/rust-analyzer", strip_top: false },
};
pub const CLANGD: Recipe =
    Recipe { command: "clangd", source: Source::Github { repo: "clangd/clangd", strip_top: true } };
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
        "clangd" | "c" | "cpp" | "c++" => Some(&CLANGD),
        "typescript-language-server" | "typescript" | "javascript" => Some(&TYPESCRIPT),
        "pyright-langserver" | "pyright" | "python" => Some(&PYRIGHT),
        "gopls" | "go" => Some(&GOPLS),
        _ => None,
    }
}

/// How to recognise the asset for this platform in a release's list: clangd
/// puts the version in the name, so a name cannot be known ahead of the
/// release, but its beginning and its end can.
#[derive(Debug, PartialEq, Eq)]
pub struct Pick {
    pub prefix: String,
    pub suffix: &'static str,
}

impl Recipe {
    /// How the machine's own tooling would install this: what a person at the
    /// keyboard would type. Runs as a command through the policy like any
    /// other, and only a `free` stance reaches for it.
    pub fn system_command(&self) -> Option<String> {
        match &self.source {
            Source::Github { .. } if self.command == "rust-analyzer" => {
                Some("rustup component add rust-analyzer".into())
            }
            Source::Github { .. } => None,
            Source::Npm { packages } => Some(format!("npm install -g {}", packages.join(" "))),
            Source::Go { module } => Some(format!("go install {module}@latest")),
        }
    }

    /// The command that installs this under `dir` and nowhere else, for the
    /// sources that are a command rather than a download. `None` is a source
    /// this fetches itself.
    pub fn command_into(&self, dir: &Path) -> Option<(String, Vec<(String, String)>)> {
        match &self.source {
            Source::Github { .. } => None,
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
            Source::Github { .. } => None,
            Source::Npm { .. } => Some("npm"),
            Source::Go { .. } => Some("go"),
        }
    }

    /// Where the binary is inside a `current` directory.
    pub fn binary_in(&self, current: &Path) -> PathBuf {
        let path = match &self.source {
            Source::Npm { .. } => current.join("node_modules").join(".bin").join(self.command),
            // clangd needs its `lib/clang` beside `bin/`, so the tree is kept
            // and the binary is where the archive put it.
            Source::Github { strip_top: true, .. } => current.join("bin").join(self.command),
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
            Source::Github { repo, .. } => (
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

    /// How to pick the asset this platform can run, or why there is none.
    pub fn picks(&self, os: &str, arch: &str, userland: &str) -> Result<Pick, String> {
        let Source::Github { repo, .. } = &self.source else {
            return Err(format!("{} is not fetched from a release", self.command));
        };
        let none = |why: &str| {
            Err(format!(
                "{} publishes no build for {why} — see https://github.com/{repo}/releases",
                self.command
            ))
        };
        match (self.command, os) {
            ("rust-analyzer", "macos") => {
                Ok(Pick { prefix: format!("rust-analyzer-{arch}-apple-darwin"), suffix: ".gz" })
            }
            ("rust-analyzer", "linux") => {
                let libc = if userland == "musl" { "musl" } else { "gnu" };
                Ok(Pick { prefix: format!("rust-analyzer-{arch}-unknown-linux-{libc}"), suffix: ".gz" })
            }
            ("rust-analyzer", "windows") => {
                Ok(Pick { prefix: format!("rust-analyzer-{arch}-pc-windows-msvc"), suffix: ".zip" })
            }
            // One build per OS: x86_64, which an arm Mac runs translated.
            ("clangd", "macos") => Ok(Pick { prefix: "clangd-mac-".into(), suffix: ".zip" }),
            ("clangd", "linux") if arch == "x86_64" => {
                Ok(Pick { prefix: "clangd-linux-".into(), suffix: ".zip" })
            }
            ("clangd", "windows") if arch == "x86_64" => {
                Ok(Pick { prefix: "clangd-windows-".into(), suffix: ".zip" })
            }
            (_, os) => none(&format!("{os} {arch}")),
        }
    }
}

/// One asset of the latest release, as its API describes it.
#[derive(Debug)]
pub struct Asset {
    pub tag: String,
    pub name: String,
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

impl Installed {
    pub fn describe(&self) -> String {
        format!("installed {} {} at {} ({})", self.command, self.tag, self.path.display(), self.verified)
    }
}

fn installed_in(into: &Path) -> Vec<(&'static Recipe, String)> {
    let Ok(entries) = std::fs::read_dir(into) else { return Vec::new() };
    let mut found: Vec<(&'static Recipe, String)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let recipe = recipe_for(&name)?;
            let tag = std::fs::read_to_string(current_in(into, recipe.command).join(".tag")).ok()?;
            Some((recipe, tag.trim().to_string()))
        })
        .collect();
    found.sort_by_key(|(recipe, _)| recipe.command);
    found
}

/// Servers under `into` whose tag was recorded longer ago than `after`, with
/// the age in days. The tag file's own age, so nothing is stored for it — and
/// a question of the directory, not of an `Installer`, because it is asked at
/// the start of every turn and a client is built only for the fetch.
pub fn stale(into: &Path, after: std::time::Duration) -> Vec<(&'static Recipe, String, u64)> {
    installed_in(into)
        .into_iter()
        .filter_map(|(recipe, tag)| {
            let recorded = current_in(into, recipe.command).join(".tag");
            let age = std::fs::metadata(recorded).ok()?.modified().ok()?.elapsed().ok()?;
            (age >= after).then_some((recipe, tag, age.as_secs() / 86_400))
        })
        .collect()
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

    /// The latest release's asset matching `pick`, with the digest the
    /// publisher listed for it.
    pub async fn latest(&self, recipe: &Recipe, pick: &Pick) -> Result<Asset, String> {
        let Source::Github { repo, .. } = &recipe.source else {
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
            .find(|a| {
                a["name"].as_str().is_some_and(|n| n.starts_with(&pick.prefix) && n.ends_with(pick.suffix))
            })
            .ok_or_else(|| {
                format!("release {tag} of {repo} has no asset named {}…{}", pick.prefix, pick.suffix)
            })?;
        let name = found["name"].as_str().unwrap_or("").to_string();
        let Some(sha256) = listed_sha256(found) else {
            return Err(format!(
                "release {tag} lists no sha256 digest for {name}, so a download could not be checked \
                 — nothing was fetched"
            ));
        };
        Ok(Asset {
            tag,
            name,
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
                let pick = recipe.picks(&env.os, &env.arch, &env.userland)?;
                let release = self.latest(recipe, &pick).await?;
                let bytes = self.download(&release).await?;
                // Nothing on disk until the bytes have matched: a refused
                // download leaves not even a directory behind.
                let versioned = self.into.join(recipe.command).join(&release.tag);
                make(&versioned)?;
                make(&current)?;
                let Source::Github { strip_top, .. } = &recipe.source else {
                    unreachable!("picked from a release")
                };
                match pick.suffix {
                    ".zip" => {
                        unpack_zip(&bytes, &versioned, *strip_top)?;
                        copy_tree(&versioned, &current)?;
                    }
                    _ => {
                        let binary = versioned.join(recipe.command);
                        unpack_gz(&bytes, &binary)?;
                        executable(&binary)?;
                        // A copy rather than a link: a link needs a privilege
                        // on Windows that an ordinary account does not have.
                        let placed = recipe.binary_in(&current);
                        std::fs::copy(&binary, &placed)
                            .map_err(|e| format!("could not put {} in place: {e}", placed.display()))?;
                        executable(&placed)?;
                    }
                }
                let (verified, unverified) = recipe.checked(&release.tag, &release.name, &release.sha256);
                (release.tag, verified, unverified)
            }
        };

        let path = recipe.binary_in(&current);
        if !path.is_file() {
            return Err(format!("{} finished but left no {} behind", recipe.command, path.display()));
        }
        // What is in place, for `update` to compare against: a server fetched
        // once is a server that is a year old a year later, and nothing else
        // says which year.
        std::fs::write(current.join(".tag"), &tag)
            .map_err(|e| format!("could not record the version: {e}"))?;
        Ok(Installed { command: recipe.command.into(), tag, path, verified, unverified })
    }

    /// Every server under this directory that has a recipe, with the tag it
    /// was installed at.
    pub fn installed(&self) -> Vec<(&'static Recipe, String)> {
        installed_in(&self.into)
    }

    /// Install again whatever is installed, and say for each whether the tag
    /// moved. A command-shaped source is always "latest" and is reinstalled
    /// rather than compared: npm and go decide what latest is.
    pub async fn update(
        &self,
        env: &rook_skills::Environment,
    ) -> Vec<(String, std::result::Result<String, String>)> {
        let mut report = Vec::new();
        for (recipe, before) in self.installed() {
            let said = match self.install(recipe, env).await {
                Ok(done) if done.tag == before && before != "latest" => Ok(format!("already at {before}")),
                Ok(_) if before == "latest" => Ok("reinstalled at latest".into()),
                Ok(done) => Ok(format!("{before} → {}", done.tag)),
                Err(why) => Err(why),
            };
            report.push((recipe.command.to_string(), said));
        }
        report
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

/// Unpack a zip into `into`, bounded on what it inflates to and refusing any
/// entry that would land outside `into`: an archive names its own paths, and
/// `../` in one is how a download writes somewhere it was not told to.
fn unpack_zip(bytes: &[u8], into: &Path, strip_top: bool) -> Result<(), String> {
    use std::io::Read;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("not a zip this can open: {e}"))?;
    let mut inflated: u64 = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| format!("entry {i} of the zip: {e}"))?;
        let Some(inside) = entry.enclosed_name() else {
            return Err(format!("the zip names a path outside itself: {:?} — refused", entry.name()));
        };
        let inside = match strip_top {
            true => inside.components().skip(1).collect::<PathBuf>(),
            false => inside,
        };
        if inside.as_os_str().is_empty() {
            continue;
        }
        let to = into.join(&inside);
        if entry.is_dir() {
            std::fs::create_dir_all(&to).map_err(|e| format!("could not create {}: {e}", to.display()))?;
            continue;
        }
        if let Some(dir) = to.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        }
        let mut out =
            std::fs::File::create(&to).map_err(|e| format!("could not write {}: {e}", to.display()))?;
        let mut chunk = vec![0u8; 64 << 10];
        loop {
            let n = entry.read(&mut chunk).map_err(|e| format!("{}: {e}", to.display()))?;
            if n == 0 {
                break;
            }
            inflated += n as u64;
            if inflated > MOST_UNPACKED_BYTES {
                return Err(format!("the archive inflates past {MOST_UNPACKED_BYTES} bytes — refused"));
            }
            std::io::Write::write_all(&mut out, &chunk[..n])
                .map_err(|e| format!("could not write {}: {e}", to.display()))?;
        }
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&to, std::fs::Permissions::from_mode(mode));
        }
    }
    Ok(())
}

/// `current` as a copy of the versioned tree, for the same reason one binary
/// is copied: a link needs a privilege on Windows that an ordinary account
/// does not have.
fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    // Everything, whatever the tree says about itself: a walker that honours
    // ignore files would leave behind whatever a package chose to hide from
    // its own repository, which is not the question here.
    let mut walk = ignore::WalkBuilder::new(from);
    walk.hidden(false).ignore(false).git_ignore(false).git_global(false).git_exclude(false).parents(false);
    for entry in walk.build().flatten() {
        let rel = entry.path().strip_prefix(from).unwrap_or(entry.path());
        let dest = to.join(rel);
        if entry.path().is_dir() {
            std::fs::create_dir_all(&dest)
                .map_err(|e| format!("could not create {}: {e}", dest.display()))?;
        } else {
            if let Some(dir) = dest.parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
            }
            std::fs::copy(entry.path(), &dest)
                .map_err(|e| format!("could not copy to {}: {e}", dest.display()))?;
        }
    }
    Ok(())
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
        let pick = |r: &Recipe, os, arch, userland| r.picks(os, arch, userland);
        assert_eq!(
            pick(&RUST_ANALYZER, "macos", "aarch64", "bsd").unwrap(),
            Pick { prefix: "rust-analyzer-aarch64-apple-darwin".into(), suffix: ".gz" }
        );
        assert_eq!(
            pick(&RUST_ANALYZER, "linux", "aarch64", "musl").unwrap().prefix,
            "rust-analyzer-aarch64-unknown-linux-musl"
        );
        assert_eq!(
            pick(&RUST_ANALYZER, "windows", "x86_64", "msvc").unwrap().suffix,
            ".zip",
            "a zip there, and it opens now"
        );
        assert_eq!(
            pick(&CLANGD, "macos", "aarch64", "bsd").unwrap().prefix,
            "clangd-mac-",
            "one mac build, run translated"
        );
        assert_eq!(
            pick(&CLANGD, "linux", "x86_64", "gnu").unwrap(),
            Pick { prefix: "clangd-linux-".into(), suffix: ".zip" }
        );
        let none = pick(&CLANGD, "linux", "aarch64", "gnu").unwrap_err();
        assert!(none.contains("no build for linux aarch64") && none.contains("releases"), "{none}");
        assert!(pick(&RUST_ANALYZER, "freebsd", "x86_64", "bsd").unwrap_err().contains("no build"));
        assert!(pick(&GOPLS, "linux", "x86_64", "gnu").unwrap_err().contains("not fetched from a release"));
    }

    fn zipped(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .unix_permissions(0o755);
        for (name, body) in entries {
            w.start_file(*name, stored).unwrap();
            w.write_all(body).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    /// The archive names its own paths, and `../` in one is a download writing
    /// somewhere it was not told to.
    #[test]
    fn a_zip_naming_a_path_outside_itself_is_refused_whole() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = zipped(&[("fine.txt", b"ok"), ("../escape.txt", b"no")]);
        let refused = unpack_zip(&bytes, dir.path(), false).unwrap_err();
        assert!(refused.contains("outside itself"), "{refused}");
        assert!(!dir.path().parent().unwrap().join("escape.txt").exists());
    }

    #[test]
    fn a_zip_is_unpacked_with_its_top_directory_dropped_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = zipped(&[
            ("clangd_1.0/bin/clangd", b"#!/bin/sh\necho clangd\n"),
            ("clangd_1.0/lib/clang/x.h", b""),
        ]);
        unpack_zip(&bytes, dir.path(), true).unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("bin").join("clangd")).unwrap(),
            b"#!/bin/sh\necho clangd\n"
        );
        assert!(dir.path().join("lib").join("clang").join("x.h").exists(), "the tree beside it is kept");
        assert!(!dir.path().join("clangd_1.0").exists(), "and the top directory is gone");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("bin").join("clangd")).unwrap().permissions().mode();
            assert_ne!(mode & 0o111, 0, "the mode the archive recorded is kept");
        }
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
