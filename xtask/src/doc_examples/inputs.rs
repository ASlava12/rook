//! Bounded, explicitly indexed repository inputs. This is admission of trusted
//! maintained examples, not a sandbox for hostile repository changes.

use super::markdown::{self, Block, Expectation};
use anyhow::{Context, Result, bail, ensure};
use semver::{Version, VersionReq};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, Metadata};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_LOCK_BYTES: usize = 512 * 1024;
const MAX_SMALL_BYTES: usize = 64 * 1024;
const REGISTRY: &str = "registry+https://github.com/rust-lang/crates.io-index";

pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub struct Input {
    pub relative: String,
    pub bytes: Vec<u8>,
    pub sha256: String,
}

pub struct Snapshot {
    pub root: PathBuf,
    pub files: BTreeMap<String, Arc<Input>>,
    total: usize,
}

pub struct Article {
    pub id: String,
    pub article: Arc<Input>,
    pub manifest: Arc<Input>,
    pub lock: Arc<Input>,
    pub blocks: Vec<Block>,
    pub package: Package,
}

pub struct Inputs {
    pub snapshot: Snapshot,
    pub articles: Vec<Article>,
}

pub struct Package {
    pub name: String,
    pub edition: u16,
    pub dependencies: Vec<Dependency>,
    pub locked: BTreeSet<(String, String)>,
}

pub struct Dependency {
    pub name: String,
    pub version: VersionReq,
}

pub fn relative(raw: &str) -> Result<()> {
    ensure!(!raw.is_empty() && raw.len() <= 1024, "invalid input path length");
    ensure!(
        !raw.chars().any(|c| c.is_control() || matches!(c, '\\' | ':'))
            && raw.split('/').all(|c| !matches!(c, "" | "." | "..")),
        "input path must contain only normal repository-relative components: {raw:?}"
    );
    ensure!(!Path::new(raw).is_absolute(), "absolute input path");
    for part in raw.split('/') {
        let stem = part.split('.').next().unwrap().to_ascii_uppercase();
        ensure!(
            !part.ends_with([' ', '.'])
                && !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                && !((stem.starts_with("COM") || stem.starts_with("LPT"))
                    && stem.len() == 4
                    && matches!(stem.as_bytes()[3], b'1'..=b'9')),
            "nonportable input component: {part:?}"
        );
    }
    Ok(())
}

fn linked(meta: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_type().is_symlink() || meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    meta.file_type().is_symlink()
}

/// Check every component, including the root, before opening a file. A changed
/// filesystem between these checks is outside the trusted-input contract.
pub fn regular(root: &Path, raw: &str) -> Result<PathBuf> {
    relative(raw)?;
    let meta = fs::symlink_metadata(root).context("reading source root metadata")?;
    ensure!(meta.is_dir() && !linked(&meta), "source root must be a regular directory");
    let mut path = root.to_path_buf();
    let mut parts = raw.split('/').peekable();
    while let Some(part) = parts.next() {
        path.push(part);
        let meta = fs::symlink_metadata(&path).with_context(|| format!("reading {raw}"))?;
        ensure!(!linked(&meta), "symlink/reparse input component: {raw}");
        ensure!(
            if parts.peek().is_some() { meta.is_dir() } else { meta.is_file() },
            "input component has the wrong file type: {raw}"
        );
    }
    Ok(path)
}

fn read(root: &Path, raw: &str, limit: usize) -> Result<Vec<u8>> {
    let path = regular(root, raw)?;
    let mut file = File::open(&path).with_context(|| format!("opening {raw}"))?;
    let before = file.metadata()?;
    ensure!(before.is_file() && !linked(&before), "input is not a regular file");
    ensure!(before.len() <= limit as u64, "input exceeds {limit} bytes: {raw}");
    let mut bytes = Vec::with_capacity(before.len() as usize);
    (&mut file).take(limit as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= limit && bytes.len() as u64 == before.len(), "input changed size: {raw}");
    ensure!(file.metadata()?.len() == before.len(), "input changed while reading: {raw}");
    Ok(bytes)
}

impl Snapshot {
    pub fn new(root: &Path) -> Result<Self> {
        let meta = fs::symlink_metadata(root).context("reading repository root")?;
        ensure!(meta.is_dir() && !linked(&meta), "repository root must be a regular directory");
        Ok(Self { root: root.canonicalize()?, files: BTreeMap::new(), total: 0 })
    }

    fn admit(&mut self, raw: &str, limit: usize) -> Result<Arc<Input>> {
        relative(raw)?;
        if let Some(input) = self.files.get(raw) {
            ensure!(input.bytes.len() <= limit, "input exceeds role limit: {raw}");
            return Ok(Arc::clone(input));
        }
        let remaining = MAX_INPUT_BYTES - self.total;
        let bytes = read(&self.root, raw, limit.min(remaining))?;
        self.total += bytes.len();
        let input = Arc::new(Input { relative: raw.into(), sha256: hash(&bytes), bytes });
        self.files.insert(raw.into(), Arc::clone(&input));
        Ok(input)
    }

    pub fn verify_unchanged(&self) -> Result<()> {
        for input in self.files.values() {
            let current = read(&self.root, &input.relative, input.bytes.len())?;
            ensure!(hash(&current) == input.sha256, "source changed: {}", input.relative);
        }
        Ok(())
    }
}

fn name(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= 64
        && raw.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
        && raw.as_bytes()[0].is_ascii_alphabetic()
}

fn fields(object: &serde_json::Map<String, Value>, allowed: &[&str]) -> Result<()> {
    for key in object.keys() {
        ensure!(allowed.contains(&key.as_str()), "unknown index field: {key}");
    }
    Ok(())
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key].as_str().with_context(|| format!("missing string field {key}"))
}

pub fn load(root: &Path, index: &str, only: &[String]) -> Result<Inputs> {
    let mut snapshot = Snapshot::new(root)?;
    let index = snapshot.admit(index, MAX_SMALL_BYTES)?;
    let index: Value = serde_json::from_slice(&index.bytes).context("parsing documentation index")?;
    fields(index.as_object().context("index must be an object")?, &["version", "articles"])?;
    ensure!(index["version"].as_u64() == Some(1), "unsupported documentation index version");
    let entries = index["articles"].as_array().context("index articles must be an array")?;
    ensure!(!entries.is_empty() && entries.len() <= 16, "index requires 1..16 articles");
    let mut ids = BTreeSet::new();
    for entry in entries {
        fields(
            entry.as_object().context("article entry must be an object")?,
            &["id", "article", "manifest", "lock", "blocks"],
        )?;
        let id = string(entry, "id")?;
        ensure!(name(id) && ids.insert(id), "invalid or duplicate article id: {id:?}");
        for key in ["article", "manifest", "lock"] {
            relative(string(entry, key)?)?;
        }
        let count = entry["blocks"].as_u64().context("blocks must be an integer")?;
        ensure!((1..=markdown::MAX_BLOCKS as u64).contains(&count), "invalid declared block count");
    }
    let mut selected = BTreeSet::new();
    for id in only {
        ensure!(ids.contains(id.as_str()), "unknown selected article: {id:?}");
        ensure!(selected.insert(id.as_str()), "duplicate selected article: {id}");
    }
    let mut articles = Vec::new();
    let mut blocks_total = 0;
    let mut code_total = 0;
    for entry in entries {
        let id = string(entry, "id")?;
        if !selected.is_empty() && !selected.contains(id) {
            continue;
        }
        let article = snapshot.admit(string(entry, "article")?, markdown::MAX_ARTICLE_BYTES)?;
        let manifest = snapshot.admit(string(entry, "manifest")?, MAX_SMALL_BYTES)?;
        let lock = snapshot.admit(string(entry, "lock")?, MAX_LOCK_BYTES)?;
        let mut package = manifest_package(&manifest.bytes).with_context(|| format!("{id}: manifest"))?;
        package.locked = lock_packages(&lock.bytes, &package).with_context(|| format!("{id}: lock"))?;
        let text = std::str::from_utf8(&article.bytes).context("article must be UTF-8")?;
        let blocks = markdown::parse(text).map_err(anyhow::Error::msg)?;
        ensure!(blocks.len() as u64 == entry["blocks"].as_u64().unwrap(), "{id}: block count changed");
        ensure!(blocks.iter().any(|b| b.expectation != Expectation::Ignore), "{id}: no checked Rust blocks");
        for block in &blocks {
            ensure!(
                block.edition.is_none_or(|e| e == package.edition),
                "{id}: fence edition differs from manifest"
            );
            blocks_total += 1;
            code_total += block.code.len();
            ensure!(
                blocks_total <= 64 && code_total <= 256 * 1024,
                "selected examples exceed aggregate code limits"
            );
        }
        articles.push(Article { id: id.into(), article, manifest, lock, blocks, package });
    }
    ensure!(!articles.is_empty(), "no articles selected");
    Ok(Inputs { snapshot, articles })
}

type Table = toml::map::Map<String, toml::Value>;

fn table_fields(table: &Table, allowed: &[&str]) -> Result<()> {
    for key in table.keys() {
        ensure!(allowed.contains(&key.as_str()), "unsupported manifest/lock field: {key}");
    }
    Ok(())
}

fn table_string<'a>(table: &'a Table, key: &str) -> Result<&'a str> {
    table.get(key).and_then(toml::Value::as_str).with_context(|| format!("missing string {key}"))
}

fn exact(raw: &str) -> Result<VersionReq> {
    let version = raw.strip_prefix('=').context("dependency requires an exact =x.y.z version")?;
    let parsed = Version::parse(version).context("dependency version must be a complete semantic version")?;
    ensure!(version == parsed.to_string(), "noncanonical dependency version");
    VersionReq::parse(raw).context("parsing exact dependency version")
}

fn dependencies(table: &Table, key: &str, output: &mut Vec<Dependency>) -> Result<()> {
    let Some(value) = table.get(key) else { return Ok(()) };
    let deps = value.as_table().context("dependencies must be a table")?;
    ensure!(deps.len() <= 32, "too many direct dependencies");
    for (alias, value) in deps {
        ensure!(name(alias), "invalid dependency alias");
        let (package, raw) = if let Some(raw) = value.as_str() {
            (alias.as_str(), raw)
        } else {
            let dep = value.as_table().context("dependency must be a version or table")?;
            table_fields(dep, &["version", "package", "features", "default-features"])?;
            if let Some(features) = dep.get("features") {
                let features = features.as_array().context("features must be an array")?;
                ensure!(features.len() <= 32, "too many features");
                for feature in features {
                    let feature = feature.as_str().context("feature must be a string")?;
                    ensure!(
                        !feature.is_empty() && feature.len() <= 128 && !feature.chars().any(char::is_control),
                        "invalid feature"
                    );
                }
            }
            if let Some(default) = dep.get("default-features") {
                ensure!(default.is_bool(), "default-features must be boolean");
            }
            let package = match dep.get("package") {
                Some(package) => package.as_str().context("dependency package must be a string")?,
                None => alias,
            };
            ensure!(name(package), "invalid dependency package name");
            (package, table_string(dep, "version")?)
        };
        output.push(Dependency { name: package.into(), version: exact(raw)? });
    }
    Ok(())
}

fn manifest_package(bytes: &[u8]) -> Result<Package> {
    let document: Table = toml::from_str(std::str::from_utf8(bytes)?)?;
    table_fields(&document, &["package", "workspace", "dependencies", "dev-dependencies"])?;
    ensure!(
        document.get("workspace").and_then(toml::Value::as_table).is_some_and(Table::is_empty),
        "manifest requires an empty independent workspace"
    );
    let package = document.get("package").and_then(toml::Value::as_table).context("missing package")?;
    table_fields(
        package,
        &[
            "name",
            "version",
            "edition",
            "publish",
            "build",
            "autolib",
            "autobins",
            "autotests",
            "autobenches",
            "autoexamples",
        ],
    )?;
    let package_name = table_string(package, "name")?;
    ensure!(name(package_name), "invalid package name");
    ensure!(table_string(package, "version")? == "0.0.0", "example package version must be 0.0.0");
    let edition = match table_string(package, "edition")? {
        "2018" => 2018,
        "2021" => 2021,
        "2024" => 2024,
        _ => bail!("unsupported example edition"),
    };
    for key in ["publish", "build", "autolib", "autobins", "autotests", "autobenches"] {
        ensure!(
            package.get(key).and_then(toml::Value::as_bool) == Some(false),
            "package {key} must be false"
        );
    }
    if let Some(value) = package.get("autoexamples") {
        ensure!(value.as_bool() == Some(true), "autoexamples must be true");
    }
    let mut dependencies_out = Vec::new();
    dependencies(&document, "dependencies", &mut dependencies_out)?;
    dependencies(&document, "dev-dependencies", &mut dependencies_out)?;
    ensure!(dependencies_out.iter().all(|d| d.name != package_name), "example cannot depend on itself");
    Ok(Package {
        name: package_name.into(),
        edition,
        dependencies: dependencies_out,
        locked: BTreeSet::new(),
    })
}

fn lock_packages(bytes: &[u8], package: &Package) -> Result<BTreeSet<(String, String)>> {
    let document: Table = toml::from_str(std::str::from_utf8(bytes)?)?;
    table_fields(&document, &["version", "package"])?;
    ensure!(document.get("version").and_then(toml::Value::as_integer) == Some(4), "lock version must be 4");
    let rows = document.get("package").and_then(toml::Value::as_array).context("missing lock packages")?;
    ensure!(!rows.is_empty() && rows.len() <= 128, "invalid lock package count");
    let mut locked: BTreeSet<(String, String)> = BTreeSet::new();
    let mut roots = 0;
    for row in rows {
        let row = row.as_table().context("lock package must be a table")?;
        table_fields(row, &["name", "version", "source", "checksum", "dependencies"])?;
        let name = table_string(row, "name")?;
        ensure!(self::name(name), "invalid locked package name");
        let version = table_string(row, "version")?;
        let parsed = Version::parse(version).context("invalid locked version")?;
        ensure!(version == parsed.to_string(), "noncanonical locked version");
        if let Some(source) = row.get("source") {
            ensure!(source.as_str() == Some(REGISTRY), "only crates.io registry locks are admitted");
            let checksum = table_string(row, "checksum")?;
            ensure!(
                checksum.len() == 64 && checksum.bytes().all(|c| c.is_ascii_hexdigit()),
                "invalid lock checksum"
            );
        } else {
            roots += 1;
            ensure!(
                name == package.name && version == "0.0.0" && !row.contains_key("checksum"),
                "unexpected local lock package"
            );
        }
        ensure!(locked.insert((name.into(), version.into())), "duplicate locked package");
        if let Some(dependencies) = row.get("dependencies") {
            let dependencies = dependencies.as_array().context("locked dependencies must be an array")?;
            ensure!(dependencies.len() <= 256, "too many locked dependency references");
            for dependency in dependencies {
                let dependency = dependency.as_str().context("invalid locked dependency reference")?;
                ensure!(
                    !dependency.is_empty()
                        && dependency.len() <= 512
                        && !dependency.chars().any(char::is_control),
                    "invalid locked dependency reference"
                );
            }
        }
    }
    ensure!(roots == 1, "lock must contain exactly one local root");
    for dependency in &package.dependencies {
        ensure!(
            locked.iter().any(|(name, version)| name == &dependency.name
                && dependency.version.matches(&Version::parse(version).expect("validated version"))),
            "dependency {} has no exact locked version",
            dependency.name
        );
    }
    Ok(locked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const MANIFEST: &str = "[package]\nname='docs_owned'\nversion='0.0.0'\nedition='2024'\npublish=false\nbuild=false\nautolib=false\nautobins=false\nautotests=false\nautobenches=false\n[workspace]\n";
    const LOCK: &str = "version=4\n[[package]]\nname='docs_owned'\nversion='0.0.0'\n";
    const ARTICLE: &str = "# Owned\n\n```rust\nfn main() {}\n```\n";

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("article.md"), ARTICLE).unwrap();
        fs::write(dir.path().join("Cargo.toml"), MANIFEST).unwrap();
        fs::write(dir.path().join("Cargo.lock"), LOCK).unwrap();
        write_index(dir.path(), json!({"version":1,"articles":[entry("owned")]}));
        dir
    }

    fn entry(id: &str) -> Value {
        json!({"id":id,"article":"article.md","manifest":"Cargo.toml","lock":"Cargo.lock","blocks":1})
    }

    fn write_index(dir: &Path, index: Value) {
        fs::write(dir.join("checks.json"), index.to_string()).unwrap();
    }

    #[test]
    fn selected_inputs_are_bounded_hashed_and_shared() {
        let dir = fixture();
        write_index(dir.path(), json!({"version":1,"articles":[entry("one"),entry("two")]}));
        let inputs = load(dir.path(), "checks.json", &[]).unwrap();
        assert_eq!(inputs.articles.len(), 2);
        assert_eq!(inputs.snapshot.files.len(), 4);
        assert!(Arc::ptr_eq(&inputs.articles[0].article, &inputs.articles[1].article));
        assert_eq!(inputs.articles[0].package.name, "docs_owned");
        assert_eq!(inputs.articles[0].article.sha256, hash(ARTICLE.as_bytes()));
        inputs.snapshot.verify_unchanged().unwrap();
        fs::write(dir.path().join("article.md"), ARTICLE.replace("main", "fail")).unwrap();
        assert!(inputs.snapshot.verify_unchanged().is_err());
    }

    #[test]
    fn all_source_roles_and_utf8_are_checked() {
        let dir = fixture();
        for (file, limit) in [
            ("checks.json", MAX_SMALL_BYTES),
            ("article.md", markdown::MAX_ARTICLE_BYTES),
            ("Cargo.toml", MAX_SMALL_BYTES),
            ("Cargo.lock", MAX_LOCK_BYTES),
        ] {
            let original = fs::read(dir.path().join(file)).unwrap();
            fs::write(dir.path().join(file), vec![b'x'; limit + 1]).unwrap();
            assert!(load(dir.path(), "checks.json", &[]).is_err(), "{file}");
            fs::write(dir.path().join(file), [0xff]).unwrap();
            assert!(load(dir.path(), "checks.json", &[]).is_err(), "{file} UTF8");
            fs::write(dir.path().join(file), original).unwrap();
        }
    }

    #[test]
    fn aggregate_budget_and_role_limit_apply_before_copying() {
        let dir = fixture();
        let mut snapshot = Snapshot::new(dir.path()).unwrap();
        snapshot.admit("article.md", 4096).unwrap();
        assert!(snapshot.admit("article.md", 1).is_err());
        snapshot.total = MAX_INPUT_BYTES - 1;
        assert!(snapshot.admit("Cargo.toml", 4096).is_err());
    }

    #[test]
    fn paths_reject_traversal_aliases_and_windows_special_names() {
        for path in [
            "",
            "/x",
            "../x",
            "x/../y",
            "x/./y",
            "./x",
            "x//y",
            "x/",
            "C:/x",
            "x\\y",
            "x:stream",
            "x\0y",
            "x\ny",
            "x.",
            "x ",
            "CON",
            "nul.rs",
            "aux.txt",
            "x/COM1.rs",
            "LPT9.txt",
        ] {
            assert!(relative(path).is_err(), "{path:?}");
        }
        for path in ["docs/a.md", "docs/пример.md", "com10.rs"] {
            relative(path).unwrap();
        }
    }

    #[test]
    fn index_shape_counts_ids_and_selection_are_strict() {
        let dir = fixture();
        for index in [
            json!({"version":2,"articles":[entry("owned")]}),
            json!({"version":1,"articles":[]}),
            json!({"version":1,"articles":[entry("owned")],"typo":true}),
            json!({"version":1,"articles":[entry("owned"),entry("owned")]}),
            json!({"version":1,"articles":[entry("../escape")]}),
        ] {
            write_index(dir.path(), index);
            assert!(load(dir.path(), "checks.json", &[]).is_err());
        }
        for (key, value) in [
            ("blocks", json!(2)),
            ("blocks", json!(0)),
            ("blocks", json!(1.5)),
            ("article", json!("../outside.md")),
            ("typo", json!(true)),
        ] {
            let mut row = entry("owned");
            row[key] = value;
            write_index(dir.path(), json!({"version":1,"articles":[row]}));
            assert!(load(dir.path(), "checks.json", &[]).is_err(), "{key}");
        }
        write_index(dir.path(), json!({"version":1,"articles":[entry("owned")]}));
        for only in [vec!["".into()], vec!["absent".into()], vec!["owned".into(), "owned".into()]] {
            assert!(load(dir.path(), "checks.json", &only).is_err());
        }
        let mut missing = entry("missing");
        missing["article"] = json!("missing.md");
        write_index(dir.path(), json!({"version":1,"articles":[entry("owned"),missing]}));
        assert!(load(dir.path(), "checks.json", &[]).is_err());
        assert!(load(dir.path(), "checks.json", &["owned".into()]).is_ok());
    }

    #[test]
    fn ignored_only_and_mismatched_editions_cannot_be_success() {
        let dir = fixture();
        for article in [
            "```rust,ignore\nfn main() {}\n```\n",
            "```rust,edition2021\nfn main() {}\n```\n",
            "# no examples",
        ] {
            fs::write(dir.path().join("article.md"), article).unwrap();
            assert!(load(dir.path(), "checks.json", &[]).is_err());
        }
    }

    #[test]
    fn manifests_refuse_build_hooks_external_targets_and_floating_dependencies() {
        manifest_package(MANIFEST.as_bytes()).unwrap();
        for extra in [
            "[build-dependencies]\nx='=1.0.0'",
            "[patch.crates-io]\nx={path='../x'}",
            "[[example]]\nname='escape'\npath='../outside.rs'",
            "[lib]\npath='../lib.rs'",
            "[dependencies]\nx='1.0.0'",
            "[dependencies]\nx='=1'",
            "[dependencies]\nx='=1.0'",
            "[dependencies]\nx={version='=1.0.0',path='../x'}",
            "[dependencies]\nx={git='https://invalid.local/x'}",
            "[dependencies]\nx={workspace=true}",
            "[dependencies]\nx={version='=1.0.0',registry='elsewhere'}",
            "[dependencies]\nx={version='=1.0.0',optional=true}",
            "[dependencies]\nx={version='=1.0.0',features=[2]}",
            "[dependencies]\ndocs_owned='=0.0.0'",
        ] {
            assert!(manifest_package(format!("{MANIFEST}\n{extra}\n").as_bytes()).is_err(), "{extra}");
        }
        for change in [
            MANIFEST.replace("build=false", "build='build.rs'"),
            MANIFEST.replace("publish=false", "publish=true"),
            MANIFEST.replace("autobins=false", "autobins=true"),
            MANIFEST.replace("[workspace]", "[workspace]\nmembers=['../outside']"),
        ] {
            assert!(manifest_package(change.as_bytes()).is_err());
        }
    }

    #[test]
    fn exact_registry_dependencies_require_valid_locked_versions_and_checksums() {
        let manifest = format!(
            "{MANIFEST}\n[dependencies]\nalias={{package='some_lib',version='=1.2.3',features=['one'],default-features=false}}\n"
        );
        let package = manifest_package(manifest.as_bytes()).unwrap();
        let lock = format!(
            "{LOCK}\n[[package]]\nname='some_lib'\nversion='1.2.3'\nsource='{REGISTRY}'\nchecksum='{}'\n",
            "a".repeat(64)
        );
        assert_eq!(lock_packages(lock.as_bytes(), &package).unwrap().len(), 2);
        for bad in [
            lock.replace("1.2.3", "1.2.4"),
            lock.replace(REGISTRY, "git+https://invalid.local/x"),
            lock.replace(&"a".repeat(64), "bad"),
            lock.replace("version=4", "version=3"),
            lock.replace("name='docs_owned'", "name='other'"),
            format!("{lock}\n[[package]]\nname='second_local'\nversion='0.0.0'\n"),
        ] {
            assert!(lock_packages(bad.as_bytes(), &package).is_err());
        }
        assert!(lock_packages(LOCK.as_bytes(), &package).is_err());
    }

    #[test]
    fn changed_or_growing_source_is_refused() {
        let dir = fixture();
        let inputs = load(dir.path(), "checks.json", &[]).unwrap();
        fs::write(dir.path().join("Cargo.lock"), format!("{LOCK}# changed\n")).unwrap();
        assert!(inputs.snapshot.verify_unchanged().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_roots_intermediate_components_and_leaf_files_are_refused() {
        use std::os::unix::fs::symlink;
        let dir = fixture();
        let outside = fixture();
        symlink(outside.path(), dir.path().join("linked")).unwrap();
        assert!(regular(dir.path(), "linked/article.md").is_err());
        assert!(Snapshot::new(&dir.path().join("linked")).is_err());
        symlink(outside.path().join("article.md"), dir.path().join("linked.md")).unwrap();
        assert!(regular(dir.path(), "linked.md").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn junction_roots_and_intermediate_components_are_refused() {
        use std::process::Command;
        use std::time::Duration;
        let dir = fixture();
        let outside = fixture();
        let junction = dir.path().join("junction");
        let mut command = Command::new("cmd.exe");
        command.args(["/d", "/c", "mklink", "/J"]).arg(&junction).arg(outside.path());
        let result = rook_contain::captured_with_cleanup(&mut command, Duration::from_secs(5), 8192).unwrap();
        assert!(result.output.status.success(), "{}", String::from_utf8_lossy(&result.output.stderr));
        assert!(regular(dir.path(), "junction/article.md").is_err());
        assert!(Snapshot::new(&junction).is_err());
        // Remove only the owned junction itself, never recurse into its target.
        fs::remove_dir(junction).unwrap();
        assert!(outside.path().join("article.md").is_file());
    }
}
