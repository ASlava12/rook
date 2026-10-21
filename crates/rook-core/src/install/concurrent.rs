//! In-process ownership of an overlapping installation, not an installed cache.
use super::{Installed, Installer, Recipe};
use futures_util::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use std::{
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

const MAX_ACTIVE: usize = 64;

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Key {
    cache: PathBuf,
    recipe: Recipe,
    api: String,
    proxy: rook_llm::Proxy,
    proxy_environment: [u8; 32],
    os: String,
    arch: String,
    userland: String,
    toolchain: Option<String>,
}

impl Key {
    pub(super) fn new(
        installer: &Installer,
        recipe: &Recipe,
        env: &rook_skills::Environment,
    ) -> Result<Self, String> {
        Ok(Self {
            cache: cache_key(&installer.into)?,
            recipe: recipe.clone(),
            api: installer.api.clone(),
            proxy: installer.proxy.clone(),
            proxy_environment: installer.proxy_environment,
            os: env.os.clone(),
            arch: env.arch.clone(),
            userland: env.userland.clone(),
            toolchain: recipe
                .needs()
                .and_then(|tool| env.tools.get(tool).or_else(|| env.languages.get(tool)))
                .cloned(),
        })
    }

    fn same_destination(&self, other: &Self) -> bool {
        self.cache == other.cache && self.recipe.command == other.recipe.command
    }
}

pub(super) struct Flight {
    key: Key,
    pub(super) work: Shared<BoxFuture<'static, Result<Installed, String>>>,
}

fn registry() -> &'static Mutex<Vec<Weak<Flight>>> {
    static ACTIVE: OnceLock<Mutex<Vec<Weak<Flight>>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(Vec::new()))
}

/// The bool says whether this is our work/result or merely a different
/// operation whose ownership of our destination must be allowed to finish.
pub(super) fn admit(
    key: &Key,
    installer: &Installer,
    recipe: &Recipe,
    env: &rook_skills::Environment,
) -> Result<(Arc<Flight>, bool), String> {
    let mut active = registry().lock().unwrap_or_else(|poison| poison.into_inner());
    // Last-owner cancellation can drop an owned command and its process group.
    // Keep every upgraded Arc alive until after releasing this short lock.
    let owners: Vec<_> = active.iter().filter_map(Weak::upgrade).collect();
    active.clear();
    active.extend(owners.iter().filter(|flight| flight.work.peek().is_none()).map(Arc::downgrade));
    let result = if let Some(flight) =
        owners.iter().find(|flight| flight.work.peek().is_none() && flight.key.same_destination(key))
    {
        let joined = flight.key == *key;
        Ok((Arc::clone(flight), joined))
    } else if active.len() >= MAX_ACTIVE {
        Err("64 language-server installations are already active; retry after one finishes".into())
    } else {
        let (installer, recipe, env) = (installer.clone(), recipe.clone(), env.clone());
        let work = async move { installer.install_once(&recipe, &env).await }.boxed().shared();
        let flight = Arc::new(Flight { key: key.clone(), work });
        active.push(Arc::downgrade(&flight));
        Ok((flight, true))
    };
    drop(active);
    drop(owners);
    result
}

/// Resolve existing ancestors without creating a cache before download
/// verification. Lexical tails let separate spellings of a new cache agree.
fn cache_key(path: &Path) -> Result<PathBuf, String> {
    let absolute =
        std::path::absolute(path).map_err(|error| format!("could not resolve install directory: {error}"))?;
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(name) => {
                resolved.push(name);
                match std::fs::canonicalize(&resolved) {
                    Ok(canonical) => resolved = canonical,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(format!("could not resolve install directory: {error}")),
                }
            }
            Component::CurDir => {}
            _ => resolved.push(component.as_os_str()),
        }
    }
    Ok(resolved)
}

/// Clients snapshot environment proxy settings when built. Keep that identity
/// without retaining or logging potentially credential-bearing environment text.
pub(super) fn proxy_environment(proxy: &rook_llm::Proxy) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    if *proxy != rook_llm::Proxy::AsTheEnvironmentSays {
        return [0; 32];
    }
    let mut hash = Sha256::new();
    for name in [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "NO_PROXY",
        "no_proxy",
        "REQUEST_METHOD",
        "GATEWAY_INTERFACE",
    ] {
        hash.update(name.as_bytes());
        match std::env::var_os(name) {
            Some(value) => {
                hash.update([1]);
                hash.update(value.as_encoded_bytes().len().to_le_bytes());
                hash.update(value.as_encoded_bytes());
            }
            None => hash.update([0]),
        }
    }
    hash.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::cache_key;

    #[test]
    fn identifying_a_missing_cache_does_not_create_it() {
        let owned = tempfile::tempdir().unwrap();
        let missing = owned.path().join("not-yet-created");
        let key = cache_key(&missing.join("child/../servers")).unwrap();
        assert_eq!(key, std::fs::canonicalize(owned.path()).unwrap().join("not-yet-created/servers"));
        assert!(!missing.exists());
    }

    #[cfg(unix)]
    #[test]
    fn parent_components_follow_the_resolved_symlink_not_its_lexical_parent() {
        let owned = tempfile::tempdir().unwrap();
        let target = owned.path().join("elsewhere/deep");
        std::fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(&target, owned.path().join("link")).unwrap();
        let actual = cache_key(&owned.path().join("missing/../link/../servers")).unwrap();
        let expected = std::fs::canonicalize(owned.path().join("elsewhere")).unwrap().join("servers");
        assert_eq!(actual, expected);
        assert_ne!(actual, cache_key(&owned.path().join("servers")).unwrap());
    }
}
