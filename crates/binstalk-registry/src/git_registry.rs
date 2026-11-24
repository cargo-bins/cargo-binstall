use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use binstalk_downloader::remote::Client;
use binstalk_types::cargo_toml_binstall::Meta;
use cargo_toml_workspace::cargo_toml::Manifest;
use compact_str::{CompactString, ToCompactString};
use fs_lock::FileLock;
use jiff::Timestamp;
use once_cell::sync::OnceCell;
use semver::VersionReq;
use serde_json::{Deserializer as JsonDeserializer, from_slice as json_from_slice};
use sha2::{Digest, Sha256};
use simple_git::{GitCancellationToken, GitUrl, Repository};
use tempfile::TempDir;
use tokio::task::spawn_blocking;
use tracing::{debug, info, instrument, warn};
use url::Url;

use crate::{
    MatchedVersion, RegistryAuth, RegistryConfig, RegistryError, crate_prefix_components,
    parse_manifest, publish_age::publish_cutoff, render_dl_template,
};

/// Keeps the on-disk clone alive (and other processes away from it) for as
/// long as the [`GitIndex`] lives.
#[derive(Debug)]
enum IndexDir {
    /// A fresh clone in a temporary directory, removed on drop.
    Temp(#[allow(dead_code)] TempDir),
    /// A clone in the cache directory, reused by later runs.
    Cached(#[allow(dead_code)] FileLock),
}

#[derive(Debug)]
struct GitIndex {
    _dir: IndexDir,
    repo: Repository,
    config: RegistryConfig,
}

impl GitIndex {
    fn new(
        url: GitUrl,
        cache_root: Option<&Path>,
        cancellation_token: GitCancellationToken,
    ) -> Result<Self, RegistryError> {
        let (dir, repo) = match cache_root {
            Some(cache_root) => open_cached(&url, cache_root, cancellation_token)?,
            None => clone_to_tempdir(&url, cancellation_token)?,
        };

        let config: RegistryConfig = {
            let config = repo
                .get_head_commit_entry_data_by_path("config.json")?
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("config.json not found in repository `{url}`"),
                    )
                })?;

            json_from_slice(&config).map_err(RegistryError::from)?
        };

        Ok(Self {
            _dir: dir,
            repo,
            config,
        })
    }
}

/// WARNING: This is a blocking operation.
fn clone_to_tempdir(
    url: &GitUrl,
    cancellation_token: GitCancellationToken,
) -> Result<(IndexDir, Repository), RegistryError> {
    let tempdir = TempDir::new()?;
    let repo =
        Repository::shallow_clone_bare(url.clone(), tempdir.as_ref(), Some(cancellation_token))?;

    Ok((IndexDir::Temp(tempdir), repo))
}

/// Open the clone of `url` cached in `cache_root`, updating it with a shallow
/// fetch, or clone it there if there is none yet.
///
/// Falls back to a clone in a temporary directory if the cache directory
/// cannot be used, e.g. because it is read-only.
///
/// WARNING: This is a blocking operation.
fn open_cached(
    url: &GitUrl,
    cache_root: &Path,
    cancellation_token: GitCancellationToken,
) -> Result<(IndexDir, Repository), RegistryError> {
    let name = cache_dir_name(url);

    let lock = match lock_cache_dir(cache_root, &name) {
        Ok(lock) => lock,
        Err(err) => {
            warn!(
                "Failed to use git index cache in {}: {err}, cloning to a temporary directory instead",
                cache_root.display()
            );
            return clone_to_tempdir(url, cancellation_token);
        }
    };

    let path = cache_root.join(&*name);

    if path.exists() {
        match update_cached(url, &path, cancellation_token.clone()) {
            Ok(Some(repo)) => return Ok((IndexDir::Cached(lock), repo)),
            Ok(None) => (),
            Err(err) => warn!(
                "Failed to update cached git index {}: {err}, cloning it again",
                path.display()
            ),
        }

        fs::remove_dir_all(&path)?;
    }

    // Clone into a temporary directory first and then move it into place, so
    // that an interrupted clone never ends up in the cache.
    let tempdir = tempfile::Builder::new()
        .prefix(&*format!("{name}.tmp"))
        .tempdir_in(cache_root)?;
    drop(Repository::shallow_clone_bare(
        url.clone(),
        tempdir.as_ref(),
        Some(cancellation_token),
    )?);
    fs::rename(tempdir.path(), &path)?;
    // Nothing is left at the temporary path for `TempDir` to remove.
    drop(tempdir);

    Ok((IndexDir::Cached(lock), Repository::open(&path)?))
}

/// Returns `Ok(None)` if the cached clone should be replaced with a new one.
///
/// WARNING: This is a blocking operation.
fn update_cached(
    url: &GitUrl,
    path: &Path,
    cancellation_token: GitCancellationToken,
) -> Result<Option<Repository>, RegistryError> {
    if has_too_much_garbage(path)? {
        debug!(
            "Cached git index {} holds too many outdated objects, cloning it again",
            path.display()
        );
        return Ok(None);
    }

    let repo = Repository::open(path)?;
    repo.shallow_fetch_head(url.clone(), Some(cancellation_token))?;

    Ok(Some(repo))
}

/// A shallow fetch only adds objects, it never removes the outdated ones and
/// there is no `git gc` in gix, so clone again once the packs added by
/// fetches outgrow the one from the initial clone.
///
/// That bounds the cache to about twice the size of a fresh clone, while the
/// full clone is only downloaded again after about as many bytes have been
/// fetched.
fn has_too_much_garbage(path: &Path) -> io::Result<bool> {
    let mut largest = 0;
    let mut total = 0;

    for entry in fs::read_dir(path.join("objects").join("pack"))? {
        let entry = entry?;
        if entry.path().extension().is_some_and(|ext| ext == "pack") {
            let size = entry.metadata()?.len();
            largest = largest.max(size);
            total += size;
        }
    }

    Ok(total - largest > largest)
}

/// Directory name for the clone of `url`, `{host}-{hash}` like cargo's
/// `registry/index/` entries, though the hash differs from cargo's.
fn cache_dir_name(url: &GitUrl) -> CompactString {
    let url = url.to_compact_string();

    let host = Url::parse(&url)
        .ok()
        .and_then(|url| {
            url.host_str()
                .filter(|host| !host.is_empty())
                .map(CompactString::from)
        })
        .unwrap_or_else(|| "local".into());
    let host: CompactString = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();

    let hash = Sha256::digest(url.as_bytes());

    format!("{host}-{}", base16::encode_lower(&hash[..8])).into()
}

/// Take an exclusive lock on the clone named `name` in `cache_root`, waiting
/// for other processes using it to finish.
///
/// WARNING: This is a blocking operation.
fn lock_cache_dir(cache_root: &Path, name: &str) -> io::Result<FileLock> {
    fs::create_dir_all(cache_root)?;

    let lock_path = cache_root.join(format!("{name}.lock"));
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)?;

    let lock = match FileLock::new_try_exclusive(file) {
        Ok(lock) => lock,
        Err((file, None)) => {
            info!(
                "Waiting for another process to finish using git index cache {}",
                lock_path.display()
            );
            FileLock::new_exclusive(file)?
        }
        Err((_, Some(err))) => return Err(err),
    };

    Ok(lock.set_file_path(lock_path))
}

#[derive(Debug)]
struct GitRegistryInner {
    url: GitUrl,
    cache_root: Option<PathBuf>,
    git_index: OnceCell<GitIndex>,
}

#[derive(Clone, Debug)]
pub struct GitRegistry(Arc<GitRegistryInner>);

impl GitRegistry {
    /// Clone the index to a temporary directory on first use.
    pub fn new(url: GitUrl) -> Self {
        Self::new_inner(url, None)
    }

    /// Keep the clone of the index in a subdirectory of `cache_root`, shared
    /// by all `GitRegistry`s using the same `cache_root`, and only fetch
    /// what changed upstream on later uses.
    ///
    /// Concurrent users of the same index wait for each other with a lock
    /// file in `cache_root`.
    pub fn with_cache(url: GitUrl, cache_root: impl Into<PathBuf>) -> Self {
        Self::new_inner(url, Some(cache_root.into()))
    }

    fn new_inner(url: GitUrl, cache_root: Option<PathBuf>) -> Self {
        Self(Arc::new(GitRegistryInner {
            url,
            cache_root,
            git_index: Default::default(),
        }))
    }

    pub fn url(&self) -> &GitUrl {
        &self.0.url
    }

    /// WARNING: This is a blocking operation.
    fn find_crate_matched_ver(
        repo: &Repository,
        crate_name: &str,
        (c1, c2): &(CompactString, Option<CompactString>),
        version_req: &VersionReq,
        cutoff: Option<Timestamp>,
    ) -> Result<MatchedVersion, RegistryError> {
        let mut path = PathBuf::with_capacity(128);
        path.push(&**c1);
        if let Some(c2) = c2 {
            path.push(&**c2);
        }

        path.push(&*crate_name.to_lowercase());
        let crate_versions = repo
            .get_head_commit_entry_data_by_path(path)?
            .ok_or_else(|| RegistryError::NotFound(crate_name.into()))?;

        MatchedVersion::find(
            &mut JsonDeserializer::from_slice(&crate_versions).into_iter(),
            version_req,
            cutoff,
        )
    }

    #[instrument(
        skip(self, client, version_req),
        fields(
            version_req = format_args!("{version_req}"),
        ),
    )]
    pub async fn fetch_crate_matched(
        &self,
        client: Client,
        auth: Option<&RegistryAuth>,
        name: &str,
        version_req: &VersionReq,
        min_publish_age: Option<Duration>,
    ) -> Result<(Manifest<Meta>, Option<CompactString>), RegistryError> {
        let cutoff = min_publish_age.and_then(|min_age| publish_cutoff(Timestamp::now(), min_age));
        let crate_prefix = crate_prefix_components(name)?;
        let crate_name = name.to_compact_string();
        let version_req = version_req.clone();
        let this = self.clone();

        let cancellation_token = GitCancellationToken::default();
        // Cancel git operation if the future is cancelled (dropped).
        let cancel_on_drop = cancellation_token.clone().cancel_on_drop();

        let (mut matched_version, dl_url, auth_required) = spawn_blocking(move || {
            let GitIndex {
                _dir: _,
                repo,
                config,
            } = this.0.git_index.get_or_try_init(|| {
                GitIndex::new(
                    this.0.url.clone(),
                    this.0.cache_root.as_deref(),
                    cancellation_token,
                )
            })?;

            let matched_version = Self::find_crate_matched_ver(
                repo,
                &crate_name,
                &crate_prefix,
                &version_req,
                cutoff,
            )?;

            let url = Url::parse(&render_dl_template(
                &config.dl,
                &crate_name,
                &crate_prefix,
                &matched_version,
            )?)?;

            Ok::<_, RegistryError>((matched_version, url, config.auth_required))
        })
        .await??;

        // Git operation done, disarm it
        cancel_on_drop.disarm();

        let auth = if auth_required {
            let Some(auth) = auth else {
                return Err(RegistryError::AuthenticationRequired(Box::new(
                    dl_url.clone(),
                )));
            };

            Some(auth)
        } else {
            None
        };

        let pubtime = matched_version.pubtime.take();
        let manifest = parse_manifest(client, name, dl_url, matched_version, auth).await?;

        Ok((manifest, pubtime))
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed: {status}");
    }

    fn commit(dir: &Path, crate_entry: Option<&str>) {
        fs::write(dir.join("config.json"), r#"{"dl": "https://example.com"}"#).unwrap();
        fs::create_dir_all(dir.join("fo/o")).unwrap();
        match crate_entry {
            Some(entry) => fs::write(dir.join("fo/o/foo"), entry).unwrap(),
            None => fs::remove_file(dir.join("fo/o/foo")).unwrap(),
        }
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "update"]);
    }

    /// Open the cached index like a new process would.
    async fn read_foo(url: &GitUrl, cache_root: &Path) -> Option<Vec<u8>> {
        let (url, cache_root) = (url.clone(), cache_root.to_owned());
        spawn_blocking(move || {
            let index = GitIndex::new(url, Some(&cache_root), Default::default()).unwrap();
            assert!(matches!(index._dir, IndexDir::Cached(_)));
            assert_eq!(index.config.dl, "https://example.com");
            index
                .repo
                .get_head_commit_entry_data_by_path("fo/o/foo")
                .unwrap()
        })
        .await
        .unwrap()
    }

    fn cache_entries(cache_root: &Path) -> Vec<String> {
        let mut entries: Vec<_> = fs::read_dir(cache_root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        entries.sort();
        entries
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cached_git_index_is_updated() {
        let upstream = TempDir::new().unwrap();
        let cache_root = TempDir::new().unwrap();
        let cache_root = cache_root.path();
        let url: GitUrl = upstream.path().to_str().unwrap().parse().unwrap();
        let name = cache_dir_name(&url);
        assert!(name.starts_with("local-"), "{name}");

        git(upstream.path(), &["init", "-q"]);
        commit(upstream.path(), Some("v1"));
        assert_eq!(
            read_foo(&url, cache_root).await.as_deref(),
            Some(&b"v1"[..])
        );
        assert_eq!(
            cache_entries(cache_root),
            [name.to_string(), format!("{name}.lock")]
        );

        // A later run sees upstream changes, including removals.
        commit(upstream.path(), Some("v2"));
        assert_eq!(
            read_foo(&url, cache_root).await.as_deref(),
            Some(&b"v2"[..])
        );
        commit(upstream.path(), None);
        assert_eq!(read_foo(&url, cache_root).await, None);
        commit(upstream.path(), Some("v3"));
        assert_eq!(
            read_foo(&url, cache_root).await.as_deref(),
            Some(&b"v3"[..])
        );

        // A broken cache is replaced with a fresh clone.
        let cached = cache_root.join(&*name);
        fs::remove_file(cached.join("HEAD")).unwrap();
        assert_eq!(
            read_foo(&url, cache_root).await.as_deref(),
            Some(&b"v3"[..])
        );
        assert_eq!(
            cache_entries(cache_root),
            [name.to_string(), format!("{name}.lock")]
        );
    }

    #[test]
    fn garbage_is_bounded_by_the_clone_size() {
        let dir = TempDir::new().unwrap();
        let pack_dir = dir.path().join("objects").join("pack");
        fs::create_dir_all(&pack_dir).unwrap();

        let add_pack = |name: &str, size: usize| {
            fs::write(pack_dir.join(format!("pack-{name}.pack")), vec![0; size]).unwrap();
            fs::write(pack_dir.join(format!("pack-{name}.idx")), vec![0; 1000]).unwrap();
        };

        add_pack("clone", 100);
        assert!(!has_too_much_garbage(dir.path()).unwrap());
        add_pack("fetch1", 60);
        assert!(!has_too_much_garbage(dir.path()).unwrap());
        add_pack("fetch2", 41);
        assert!(has_too_much_garbage(dir.path()).unwrap());
    }

    #[test]
    fn cache_dir_names() {
        let name = |url: &str| cache_dir_name(&url.parse().unwrap());

        let crates_io = name("https://github.com/rust-lang/crates.io-index");
        assert!(crates_io.starts_with("github.com-"), "{crates_io}");
        assert_eq!(crates_io.len(), "github.com-".len() + 16);
        assert_ne!(crates_io, name("https://github.com/rust-lang/other-index"));
        assert!(name("file:///tmp/index").starts_with("local-"));
        assert!(name("ssh://git@example.com:22/index").starts_with("example.com-"));
    }
}
