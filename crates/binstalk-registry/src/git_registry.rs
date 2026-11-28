use std::{
    fs,
    hash::{Hash, Hasher},
    io,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use binstalk_downloader::remote::Client;
use binstalk_types::redact_displayed_url_userinfo;
use compact_str::{CompactString, ToCompactString};
use fs_lock::FileLock;
use jiff::Timestamp;
use once_cell::sync::OnceCell;
use rustc_stable_hash::StableSipHasher128;
use semver::VersionReq;
use serde_json::{Deserializer as JsonDeserializer, from_slice as json_from_slice};
use simple_git::{GitCancellationToken, GitUrl, Repository};
use tempfile::TempDir;
use tokio::task::spawn_blocking;
use tracing::{debug, info, instrument, warn};
use url::Url;

use crate::{
    MatchedCrate, MatchedVersion, RegistryAuth, RegistryConfig, RegistryError,
    crate_prefix_components, publish_age::publish_cutoff, render_dl_template,
};

/// Where the index is read from.
#[derive(Debug)]
enum IndexDir {
    /// The index's own repository on this machine (a `file://` index), read
    /// in place at its `HEAD`, the commit cargo would fetch from it.
    Local,
    /// A fresh clone in a temporary directory, removed on drop.
    Temp(#[allow(dead_code)] TempDir),
    /// A clone in cargo's `$CARGO_HOME/registry/index/`, shared with cargo.
    /// It must only be used while holding [`lock_package_cache`].
    Cargo { cargo_home: PathBuf },
}

#[derive(Debug)]
struct GitIndex {
    dir: IndexDir,
    repo: Repository,
    config: RegistryConfig,
}

impl GitIndex {
    /// Read a `file://` index in place, and clone any other one.
    ///
    /// WARNING: This is a blocking operation.
    fn new(
        url: GitUrl,
        cargo_url: Option<&Url>,
        cargo_home: Option<&Path>,
        cancellation_token: GitCancellationToken,
    ) -> Result<Self, RegistryError> {
        if let Some(repo) = cargo_url.and_then(open_local) {
            return Self::with_repo(&url, IndexDir::Local, repo);
        }

        Self::cloned(url, cargo_url, cargo_home, cancellation_token)
    }

    /// WARNING: This is a blocking operation.
    fn cloned(
        url: GitUrl,
        cargo_url: Option<&Url>,
        cargo_home: Option<&Path>,
        cancellation_token: GitCancellationToken,
    ) -> Result<Self, RegistryError> {
        let shared = cargo_home.and_then(|cargo_home| {
            match open_cargo_clone(&url, cargo_url, cargo_home, cancellation_token.clone()) {
                Ok(repo) => repo.map(|repo| {
                    let dir = IndexDir::Cargo {
                        cargo_home: cargo_home.to_owned(),
                    };
                    (dir, repo)
                }),
                Err(err) => {
                    warn!(
                        "Failed to use cargo's clone of git index {} in {}: {err}, cloning to a temporary directory instead",
                        redact_displayed_url_userinfo(&url),
                        cargo_home.display()
                    );
                    None
                }
            }
        });
        let (dir, repo) = match shared {
            Some(shared) => shared,
            None => clone_to_tempdir(&url, cancellation_token)?,
        };

        Self::with_repo(&url, dir, repo)
    }

    /// WARNING: This is a blocking operation.
    fn with_repo(url: &GitUrl, dir: IndexDir, repo: Repository) -> Result<Self, RegistryError> {
        let config: RegistryConfig = {
            let _lock = dir.lock()?;
            let config = repo
                .get_entry_data_by_ref_and_path(dir.reference(), "config.json")?
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!(
                            "config.json not found in repository `{}`",
                            redact_displayed_url_userinfo(url)
                        ),
                    )
                })?;

            json_from_slice(&config).map_err(RegistryError::from)?
        };

        Ok(Self { dir, repo, config })
    }
}

impl IndexDir {
    /// Lock what needs locking before reading the clone.
    ///
    /// WARNING: This is a blocking operation.
    fn lock(&self) -> io::Result<Option<PackageCacheLock>> {
        match self {
            Self::Local | Self::Temp(_) => Ok(None),
            Self::Cargo { cargo_home } => lock_package_cache(cargo_home).map(Some),
        }
    }

    /// The reference to read the index at.
    fn reference(&self) -> &'static str {
        match self {
            Self::Local => "HEAD",
            Self::Temp(_) | Self::Cargo { .. } => Repository::FETCHED_HEAD,
        }
    }
}

/// Open the repository of a `file://` index, which is already on this
/// machine, so there is nothing to clone or cache.
///
/// WARNING: This is a blocking operation.
fn open_local(url: &Url) -> Option<Repository> {
    if url.scheme() != "file" {
        return None;
    }
    let path = url.to_file_path().ok()?;

    Repository::open(&path)
        .inspect_err(|err| {
            debug!(
                "Cannot read git index {url} in place from {}: {err}, cloning it instead",
                path.display()
            )
        })
        .ok()
}

/// WARNING: This is a blocking operation.
fn clone_to_tempdir(
    url: &GitUrl,
    cancellation_token: GitCancellationToken,
) -> Result<(IndexDir, Repository), RegistryError> {
    let tempdir = TempDir::new()?;
    let repo = Repository::init(tempdir.path())?;
    repo.fetch_head(url.clone(), true, Some(cancellation_token))?;

    Ok((IndexDir::Temp(tempdir), repo))
}

/// Open and update cargo's clone of the index `url`, the same one
/// `cargo install`/`cargo build` use, cloning it if cargo has none yet.
///
/// Like cargo, this uses `$CARGO_HOME/registry/index/{host}-{hash}` if it
/// exists, which is a full clone unless `-Zgit=shallow-index` was used to
/// create it, and otherwise the shallow clone in `{host}-{hash}-shallow`,
/// which is what cargo uses with `-Zgit=shallow-index`. A new clone is always
/// a shallow one, because a full clone can be orders of magnitude larger.
///
/// Returns `Ok(None)` if `url` cannot be mapped to cargo's directory name.
///
/// WARNING: This is a blocking operation.
fn open_cargo_clone(
    url: &GitUrl,
    cargo_url: Option<&Url>,
    cargo_home: &Path,
    cancellation_token: GitCancellationToken,
) -> Result<Option<Repository>, RegistryError> {
    let Some(name) = cargo_url.and_then(|cargo_url| cargo_index_dir_name(cargo_url.clone())) else {
        debug!(
            "Cannot share cargo's clone of git index {}: not a URL cargo accepts",
            redact_displayed_url_userinfo(url)
        );
        return Ok(None);
    };
    let index_root = cargo_home.join("registry").join("index");

    let _lock = lock_package_cache(cargo_home)?;

    let full = index_root.join(&*name);
    if full.exists() {
        match fetch_existing(url, &full, cancellation_token.clone()) {
            Ok(repo) => return Ok(Some(repo)),
            // It is cargo's to repair, a full clone is too expensive for us.
            Err(err) => warn!(
                "Failed to update cargo's clone of git index {} in {}: {err}",
                redact_displayed_url_userinfo(url),
                full.display()
            ),
        }
    }

    let shallow = index_root.join(format!("{name}-shallow"));
    if shallow.exists() {
        if has_too_much_garbage(&shallow) {
            debug!(
                "Shallow clone {} holds too many outdated objects, cloning it again",
                shallow.display()
            );
        } else {
            match fetch_existing(url, &shallow, cancellation_token.clone()) {
                Ok(repo) => return Ok(Some(repo)),
                Err(err) => warn!(
                    "Failed to update shallow clone of git index {} in {}: {err}, cloning it again",
                    redact_displayed_url_userinfo(url),
                    shallow.display()
                ),
            }
        }

        // Cargo does the same when it cannot open the clone.
        fs::remove_dir_all(&shallow)?;
    }

    fs::create_dir_all(&shallow)?;
    let repo = Repository::init(&shallow)?;
    repo.fetch_head(url.clone(), true, Some(cancellation_token))?;

    Ok(Some(repo))
}

/// Fetch into an existing clone, keeping it shallow or full like cargo does.
///
/// WARNING: This is a blocking operation.
fn fetch_existing(
    url: &GitUrl,
    path: &Path,
    cancellation_token: GitCancellationToken,
) -> Result<Repository, RegistryError> {
    let repo = Repository::open(path)?;
    let shallow = repo.is_shallow()?;
    if repo.fetch_head(url.clone(), shallow, Some(cancellation_token))? {
        maybe_gc(&repo);
    }

    Ok(repo)
}

/// Run `git gc --auto` after a fetch, as cargo does, so that packs do not
/// pile up. If git is not installed this does nothing, and
/// [`has_too_much_garbage`] bounds the size of shallow clones instead.
fn maybe_gc(repo: &Repository) {
    let git_dir = repo.git_dir();
    match Command::new("git")
        .args(["gc", "--auto", "--quiet"])
        // Like cargo, so `safe.bareRepository=explicit` doesn't reject it.
        .env("GIT_DIR", git_dir)
        .current_dir(git_dir)
        .output()
    {
        Ok(output) if output.status.success() => (),
        Ok(output) => debug!(
            "git gc --auto failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(err) => debug!("Failed to run git gc --auto: {err}"),
    }
}

/// A fetch only adds objects, it never removes the outdated ones, so once
/// the packs added by fetches outgrow the largest one, i.e. the one from the
/// initial clone, clone again.
///
/// That bounds a shallow clone to about twice the size of a fresh one when
/// `git gc` is not available.
fn has_too_much_garbage(path: &Path) -> bool {
    let pack_dir = path.join(".git").join("objects").join("pack");
    let Ok(entries) = fs::read_dir(pack_dir) else {
        return false;
    };

    let mut largest = 0;
    let mut total = 0;
    for entry in entries.flatten() {
        if entry.path().extension().is_some_and(|ext| ext == "pack") {
            let size = entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
            largest = largest.max(size);
            total += size;
        }
    }

    total - largest > largest
}

/// Cargo's directory name in `$CARGO_HOME/registry/index/` for the git
/// index `url`: `{host}-{hash}`, where `hash` is cargo's `short_hash` of the
/// registry's `SourceId`.
///
/// Cargo promises to keep this stable, see `short_name` in cargo's
/// `sources/registry/mod.rs` and its `test_stable_hash` test, whose values
/// the tests here check against.
fn cargo_index_dir_name(url: Url) -> Option<CompactString> {
    let url = canonicalize_url(url)?;

    // `SourceId`'s `Hash`: its `SourceKind` (a fieldless variant hashes just
    // its discriminant, `Registry` is the third one), then its canonical URL.
    let mut hasher = StableSipHasher128::new();
    2isize.hash(&mut hasher);
    url.as_str().hash(&mut hasher);
    let hash = Hasher::finish(&hasher).to_le_bytes();

    Some(
        format!(
            "{}-{}",
            url.host_str().unwrap_or(""),
            base16::encode_lower(&hash)
        )
        .into(),
    )
}

/// cargo's `CanonicalUrl::new`.
fn canonicalize_url(mut url: Url) -> Option<Url> {
    if url.cannot_be_a_base() {
        return None;
    }

    if url.path().ends_with('/') {
        url.path_segments_mut().ok()?.pop_if_empty();
    }

    if !url.scheme().contains('+') {
        if url.host_str() == Some("github.com") {
            url = format!("https{}", &url[url::Position::AfterScheme..])
                .parse()
                .ok()?;
            let path = url.path().to_lowercase();
            url.set_path(&path);
        }

        if let Some(last) = url
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .and_then(|last| last.strip_suffix(".git"))
        {
            let last = last.to_owned();
            url.path_segments_mut().ok()?.pop().push(&last);
        }
    }

    Some(url)
}

/// Cargo's package cache lock, see [`lock_package_cache`].
struct PackageCacheLock {
    _file: FileLock,
    _guard: MutexGuard<'static, ()>,
}

/// Take cargo's package cache lock (`CacheLockMode::DownloadExclusive`),
/// which cargo holds while resolving and downloading, including when
/// updating and reading its registry index clones.
///
/// It also keeps cargo's cache garbage collection away, which takes this
/// lock as well before deleting anything.
///
/// WARNING: This is a blocking operation.
fn lock_package_cache(cargo_home: &Path) -> io::Result<PackageCacheLock> {
    // The file lock is per open file, so serialise this process's users
    // first: they would block each other on it anyway, and the message
    // below is only meant for waiting on other processes.
    static IN_PROCESS: Mutex<()> = Mutex::new(());
    let guard = IN_PROCESS.lock().unwrap_or_else(PoisonError::into_inner);

    fs::create_dir_all(cargo_home)?;

    let lock_path = cargo_home.join(".package-cache");
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?;

    let lock = match FileLock::new_try_exclusive(file) {
        Ok(lock) => lock,
        Err((file, None)) => {
            info!(
                "Waiting for cargo's package cache lock {}",
                lock_path.display()
            );
            FileLock::new_exclusive(file)?
        }
        Err((_, Some(err))) => return Err(err),
    };

    Ok(PackageCacheLock {
        _file: lock.set_file_path(lock_path),
        _guard: guard,
    })
}

#[derive(Debug)]
struct GitRegistryInner {
    url: GitUrl,
    /// The URL as cargo would parse it, for [`cargo_index_dir_name`].
    cargo_url: Option<Url>,
    cargo_home: Option<PathBuf>,
    git_index: OnceCell<GitIndex>,
}

#[derive(Clone, Debug)]
pub struct GitRegistry(Arc<GitRegistryInner>);

impl GitRegistry {
    /// Clone the index to a temporary directory on first use, or read it in
    /// place for a `file://` index.
    pub fn new(url: GitUrl) -> Self {
        let cargo_url = Url::parse(&url.to_compact_string()).ok();
        Self::new_inner(url, cargo_url, None)
    }

    /// Like [`GitRegistry::new`], keeping `index` as written by the user
    /// (e.g. in cargo's config) so that it maps to the same directory in
    /// `$CARGO_HOME/registry/index/` as for cargo.
    pub(crate) fn from_index_str(url: GitUrl, index: &str) -> Self {
        Self::new_inner(url, Url::parse(index).ok(), None)
    }

    /// Share cargo's clone of the index in `cargo_home`
    /// (`$CARGO_HOME/registry/index/`), only fetching what changed upstream,
    /// and leave a shallow clone there for later runs if cargo has none.
    ///
    /// Takes cargo's package cache lock while updating or reading the clone,
    /// so it waits for, and blocks, cargo resolving or downloading meanwhile.
    ///
    /// A `file://` index is not cloned at all, it is read in place.
    pub fn with_cargo_home(self, cargo_home: impl Into<PathBuf>) -> Self {
        Self::new_inner(
            self.0.url.clone(),
            self.0.cargo_url.clone(),
            Some(cargo_home.into()),
        )
    }

    fn new_inner(url: GitUrl, cargo_url: Option<Url>, cargo_home: Option<PathBuf>) -> Self {
        Self(Arc::new(GitRegistryInner {
            url,
            cargo_url,
            cargo_home,
            git_index: Default::default(),
        }))
    }

    pub fn url(&self) -> &GitUrl {
        &self.0.url
    }

    /// WARNING: This is a blocking operation.
    fn find_crate_matched_ver(
        repo: &Repository,
        reference: &str,
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
            .get_entry_data_by_ref_and_path(reference, path)?
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
    pub async fn find_crate_matched(
        &self,
        client: Client,
        auth: Option<&RegistryAuth>,
        name: &str,
        version_req: &VersionReq,
        min_publish_age: Option<Duration>,
    ) -> Result<MatchedCrate, RegistryError> {
        let cutoff = min_publish_age.and_then(|min_age| publish_cutoff(Timestamp::now(), min_age));
        let crate_prefix = crate_prefix_components(name)?;
        let crate_name = name.to_compact_string();
        let version_req = version_req.clone();
        let this = self.clone();

        let cancellation_token = GitCancellationToken::default();
        // Cancel git operation if the future is cancelled (dropped).
        let cancel_on_drop = cancellation_token.clone().cancel_on_drop();

        let (matched_version, dl_url, auth_required) = spawn_blocking(move || {
            let GitIndex { dir, repo, config } = this.0.git_index.get_or_try_init(|| {
                GitIndex::new(
                    this.0.url.clone(),
                    this.0.cargo_url.as_ref(),
                    this.0.cargo_home.as_deref(),
                    cancellation_token,
                )
            })?;
            let _lock = dir.lock()?;

            let matched_version = Self::find_crate_matched_ver(
                repo,
                dir.reference(),
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

        Ok(MatchedCrate::new(
            client,
            name,
            dl_url,
            matched_version,
            auth,
        ))
    }
}

#[cfg(test)]
mod tests {
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

    struct Upstream {
        dir: TempDir,
        url: GitUrl,
        cargo_url: Url,
    }

    fn upstream() -> Upstream {
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init", "-q"]);
        commit(dir.path(), Some("v1"));
        let cargo_url = Url::from_directory_path(dir.path()).unwrap();
        let url = cargo_url.as_str().parse().unwrap();
        Upstream {
            dir,
            url,
            cargo_url,
        }
    }

    /// Open the index in `cargo_home` like a new process would, cloning it as
    /// for a remote index although `upstream` is a local one.
    async fn read_foo(upstream: &Upstream, cargo_home: &Path) -> Option<Vec<u8>> {
        let (url, cargo_url) = (upstream.url.clone(), upstream.cargo_url.clone());
        let cargo_home = cargo_home.to_owned();
        spawn_blocking(move || {
            let index =
                GitIndex::cloned(url, Some(&cargo_url), Some(&cargo_home), Default::default())
                    .unwrap();
            assert!(matches!(index.dir, IndexDir::Cargo { .. }));
            assert_eq!(index.config.dl, "https://example.com");
            let _lock = index.dir.lock().unwrap();
            index
                .repo
                .get_entry_data_by_ref_and_path(Repository::FETCHED_HEAD, "fo/o/foo")
                .unwrap()
        })
        .await
        .unwrap()
    }

    fn index_entries(cargo_home: &Path) -> Vec<String> {
        let mut entries: Vec<_> = fs::read_dir(cargo_home.join("registry").join("index"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        entries.sort();
        entries
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn new_shallow_clone_is_updated() {
        let upstream = upstream();
        let cargo_home = TempDir::new().unwrap();
        let cargo_home = cargo_home.path();
        let name = cargo_index_dir_name(upstream.cargo_url.clone()).unwrap();
        // A `file://` URL has no host.
        assert!(name.starts_with('-'), "{name}");
        let shallow_name = format!("{name}-shallow");

        assert_eq!(read_foo(&upstream, cargo_home).await.unwrap(), b"v1");
        assert_eq!(index_entries(cargo_home), [&*shallow_name]);
        assert!(cargo_home.join(".package-cache").exists());
        let shallow = cargo_home.join("registry/index").join(&shallow_name);
        assert!(Repository::open(&shallow).unwrap().is_shallow().unwrap());

        // A later run sees upstream changes, including removals.
        commit(upstream.dir.path(), Some("v2"));
        assert_eq!(read_foo(&upstream, cargo_home).await.unwrap(), b"v2");
        commit(upstream.dir.path(), None);
        assert_eq!(read_foo(&upstream, cargo_home).await, None);
        commit(upstream.dir.path(), Some("v3"));
        assert_eq!(read_foo(&upstream, cargo_home).await.unwrap(), b"v3");

        // A broken clone is replaced with a fresh one.
        fs::remove_file(shallow.join(".git").join("HEAD")).unwrap();
        assert_eq!(read_foo(&upstream, cargo_home).await.unwrap(), b"v3");
        assert_eq!(index_entries(cargo_home), [&*shallow_name]);
    }

    /// A full clone made the way cargo makes one is used, and kept full.
    #[tokio::test(flavor = "multi_thread")]
    async fn cargo_full_clone_is_reused() {
        let upstream = upstream();
        let cargo_home = TempDir::new().unwrap();
        let cargo_home = cargo_home.path();
        let name = cargo_index_dir_name(upstream.cargo_url.clone()).unwrap();

        let full = cargo_home.join("registry/index").join(&*name);
        fs::create_dir_all(&full).unwrap();
        git(&full, &["init", "-q"]);
        git(
            &full,
            &[
                "fetch",
                "-q",
                &upstream.url.to_string(),
                "+HEAD:refs/remotes/origin/HEAD",
            ],
        );

        commit(upstream.dir.path(), Some("v2"));
        assert_eq!(read_foo(&upstream, cargo_home).await.unwrap(), b"v2");
        assert_eq!(index_entries(cargo_home), [&*name]);
        assert!(!Repository::open(&full).unwrap().is_shallow().unwrap());
    }

    /// A `file://` index is read in place, so cargo's directory is untouched
    /// and new commits are seen without fetching.
    #[tokio::test(flavor = "multi_thread")]
    async fn local_index_is_read_in_place() {
        let upstream = upstream();
        let cargo_home = TempDir::new().unwrap();
        let read_foo = || {
            let registry =
                GitRegistry::new(upstream.url.clone()).with_cargo_home(cargo_home.path());
            spawn_blocking(move || {
                let index = registry
                    .0
                    .git_index
                    .get_or_try_init(|| {
                        GitIndex::new(
                            registry.0.url.clone(),
                            registry.0.cargo_url.as_ref(),
                            registry.0.cargo_home.as_deref(),
                            Default::default(),
                        )
                    })
                    .unwrap();
                assert!(matches!(index.dir, IndexDir::Local));
                assert_eq!(index.config.dl, "https://example.com");
                index
                    .repo
                    .get_entry_data_by_ref_and_path(index.dir.reference(), "fo/o/foo")
                    .unwrap()
            })
        };

        assert_eq!(read_foo().await.unwrap().unwrap(), b"v1");
        commit(upstream.dir.path(), Some("v2"));
        assert_eq!(read_foo().await.unwrap().unwrap(), b"v2");
        assert!(!cargo_home.path().join("registry").exists());
        assert!(!cargo_home.path().join(".package-cache").exists());
    }

    #[test]
    fn garbage_is_bounded_by_the_clone_size() {
        let dir = TempDir::new().unwrap();
        let pack_dir = dir.path().join(".git").join("objects").join("pack");
        fs::create_dir_all(&pack_dir).unwrap();

        let add_pack = |name: &str, size: usize| {
            fs::write(pack_dir.join(format!("pack-{name}.pack")), vec![0; size]).unwrap();
            fs::write(pack_dir.join(format!("pack-{name}.idx")), vec![0; 1000]).unwrap();
        };

        add_pack("clone", 100);
        assert!(!has_too_much_garbage(dir.path()));
        add_pack("fetch1", 60);
        assert!(!has_too_much_garbage(dir.path()));
        add_pack("fetch2", 41);
        assert!(has_too_much_garbage(dir.path()));
    }

    /// The values from cargo's `test_stable_hash` (`SourceId::for_registry`
    /// and `for_alt_registry` hash the same way), and the crates.io git
    /// index's well known `github.com-25cdd57fae9f0462`.
    #[test]
    fn cargo_index_dir_names() {
        let name = |url: &str| cargo_index_dir_name(Url::parse(url).unwrap());

        let crates_io = "github.com-25cdd57fae9f0462";
        assert_eq!(
            name("https://github.com/rust-lang/crates.io-index").unwrap(),
            crates_io
        );
        assert_eq!(
            name("https://github.com/Rust-Lang/crates.io-index.git/").unwrap(),
            crates_io
        );
        assert_eq!(
            name("https://my-crates.io").unwrap(),
            "my-crates.io-b2d65deb64f05373"
        );
        assert_eq!(
            name("https://your-crates.io").unwrap(),
            "your-crates.io-755952de063f5dc4"
        );
        // scp-like, not a URL cargo accepts.
        assert!(Url::parse("git@github.com:rust-lang/crates.io-index").is_err());
    }
}
