//! Find the libraries a binary needs the way glibc's and musl's dynamic
//! loaders do, without loading them.
//!
//! The search errs on the side of finding: it looks in every directory
//! the loader might, in roughly the loader's order, and anything it can't
//! follow makes "not found" inconclusive rather than a verdict.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    env, fs,
    path::{Path, PathBuf},
};

use super::elf::{self, Elf};

/// glibc's configuration of where `ldconfig` looks. The loader doesn't
/// read it, only [`LD_SO_CACHE`], but a glibc without it may be
/// configured another way, e.g. NixOS' nix-ld.
const LD_SO_CONF: &str = "/etc/ld.so.conf";
/// glibc's cache of what `ldconfig` found, which the loader looks
/// libraries up in before its trusted directories.
const LD_SO_CACHE: &str = "/etc/ld.so.cache";
/// musl's default path, when `/etc/ld-musl-$ARCH.path` doesn't exist.
const MUSL_DEFAULT_PATH: &str = "/lib:/usr/local/lib:/usr/lib";
/// Stop following dependencies after this many libraries: a real
/// program needs far fewer, so a longer chain is a malformed or hostile
/// one.
const MAX_LIBRARIES: usize = 1024;

/// The loader the binary asks for, by how it looks up libraries.
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub(crate) enum Flavor {
    #[default]
    Glibc,
    /// musl, with the `$ARCH` from its loader name `ld-musl-$ARCH.so.1`.
    Musl { arch: String },
}

impl Flavor {
    /// The flavor of the loader at `interp`, if it is glibc or musl.
    pub(crate) fn of(interp: &str) -> Option<Self> {
        let name = super::file_name(interp);
        if let Some(arch) = name
            .strip_prefix("ld-musl-")
            .and_then(|rest| rest.strip_suffix(".so.1"))
        {
            Some(Self::Musl { arch: arch.into() })
        } else if name.starts_with("ld-linux") || name.starts_with("ld64.so.") || name == "ld.so.1"
        {
            Some(Self::Glibc)
        } else {
            None
        }
    }
}

/// Where the loader searches after the binary's own rpaths and
/// `LD_LIBRARY_PATH`, and whether that is known completely.
#[derive(Debug, Default)]
pub(crate) struct SystemPath {
    flavor: Flavor,
    /// The contents of [`LD_SO_CACHE`], to look names up in (glibc).
    cache: Vec<u8>,
    dirs: Vec<PathBuf>,
    /// Whether every part of the configuration could be read, so that
    /// a library not found in `dirs` can't be found by the loader
    /// either.
    complete: bool,
}

impl SystemPath {
    pub(crate) fn read(flavor: &Flavor) -> Self {
        match flavor {
            Flavor::Glibc => Self::glibc(),
            Flavor::Musl { arch } => Self::musl(arch.clone()),
        }
    }

    fn glibc() -> Self {
        let cache = fs::read(LD_SO_CACHE);
        let mut path = Self {
            complete: cache.is_ok() && Path::new(LD_SO_CONF).exists(),
            cache: cache.unwrap_or_default(),
            ..Self::default()
        };

        // The trusted directories, built into glibc: `/lib` and
        // `/usr/lib`, `lib64` on some 64-bit targets, and the multiarch
        // directories on Debian and its derivatives. Looking in all of
        // them is harmless, as a library for another architecture
        // doesn't count.
        for parent in ["/lib", "/usr/lib"] {
            if let Ok(entries) = fs::read_dir(parent) {
                let mut multiarch: Vec<_> = entries
                    .flatten()
                    .filter(|entry| {
                        entry
                            .file_name()
                            .to_str()
                            .is_some_and(|name| name.contains("-linux-") && entry.path().is_dir())
                    })
                    .map(|entry| entry.path())
                    .collect();
                multiarch.sort();
                path.dirs.extend(multiarch);
            }
        }
        path.dirs
            .extend(["/lib64", "/usr/lib64", "/lib", "/usr/lib"].map(PathBuf::from));
        path
    }

    fn musl(arch: String) -> Self {
        let (path, complete) = match fs::read_to_string(format!("/etc/ld-musl-{arch}.path")) {
            Ok(path) => (path, true),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                (MUSL_DEFAULT_PATH.to_owned(), true)
            }
            Err(_) => (MUSL_DEFAULT_PATH.to_owned(), false),
        };
        Self {
            dirs: path
                .split([':', '\n'])
                .filter(|dir| !dir.is_empty())
                .map(PathBuf::from)
                .collect(),
            complete,
            flavor: Flavor::Musl { arch },
            ..Self::default()
        }
    }

    /// Paths [`LD_SO_CACHE`] lists for the library `name`.
    fn cached(&self, name: &str) -> impl Iterator<Item = PathBuf> {
        // The cache's string table holds each library's name and its
        // full path, NUL-terminated. Rather than parse its two formats,
        // look for "/{name}\0" and take the path it ends.
        let needle = format!("/{name}\0").into_bytes();
        let cache = &self.cache;
        let mut paths = Vec::new();
        let mut start = 0;
        while let Some(pos) = find(&cache[start..], &needle) {
            let end = start + pos + needle.len() - 1;
            let begin = cache[..end]
                .iter()
                .rposition(|&b| b == 0)
                .map_or(0, |nul| nul + 1);
            if let Ok(path) = std::str::from_utf8(&cache[begin..end])
                && path.starts_with('/')
            {
                paths.push(PathBuf::from(path));
            }
            start = end + 1;
        }
        paths.into_iter()
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// What the search found.
#[derive(Debug, Default)]
pub(crate) struct Libraries {
    /// The libraries found, by the name they were needed as: their
    /// path, and the version names they define.
    pub(crate) found: HashMap<String, (PathBuf, Vec<String>)>,
    /// A library the loader certainly can't find, if any. The search
    /// stops there, so `found` is then incomplete.
    pub(crate) missing: Option<String>,
}

/// A loaded object: the binary, or a library found for it.
struct Object {
    rpath: Vec<String>,
    runpath: Vec<String>,
    /// The object that needed it, `None` for the binary.
    needed_by: Option<usize>,
}

/// The directories in the rpath or runpath entries `entries` that don't
/// depend on where the binary ends up: not `$ORIGIN` and friends, and
/// not relative.
fn absolute(entries: &[String]) -> impl Iterator<Item = PathBuf> + '_ {
    entries
        .iter()
        .flat_map(|entry| entry.split(':'))
        .filter(|dir| dir.starts_with('/') && !dir.contains('$'))
        .map(PathBuf::from)
}

/// Find the libraries `binary` needs, recursively.
///
/// `skip` names libraries the loader provides itself. A library is
/// reported missing only if `can_tell_missing`, the search path is
/// complete, and neither the object needing it nor any object that
/// (indirectly) needed that one has an rpath: rpaths can depend on
/// where the binary is installed (`$ORIGIN`, relative paths), and glibc
/// and musl each search their chain of rpaths differently.
pub(crate) fn libraries(
    binary: &Elf,
    system: &SystemPath,
    can_tell_missing: bool,
    skip: impl Fn(&str) -> bool,
) -> Libraries {
    let ld_library_path: Vec<PathBuf> = env::var_os("LD_LIBRARY_PATH")
        .map(|path| {
            path.to_string_lossy()
                .split([':', ';'])
                .filter(|dir| !dir.is_empty())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default();

    let musl = matches!(system.flavor, Flavor::Musl { .. });
    let mut libraries = Libraries::default();
    let mut objects = vec![Object {
        rpath: binary.rpath.clone(),
        runpath: binary.runpath.clone(),
        needed_by: None,
    }];
    let mut seen = HashSet::new();
    let mut queue: VecDeque<_> = binary.needed.iter().map(|name| (name.clone(), 0)).collect();

    while let Some((name, needed_by)) = queue.pop_front() {
        if skip(&name) || !seen.insert(name.clone()) {
            continue;
        }
        if seen.len() > MAX_LIBRARIES {
            break;
        }

        // The object needing it, then the one that needed that, up to
        // the binary.
        let chain: Vec<&Object> = std::iter::successors(Some(&objects[needed_by]), |object| {
            object.needed_by.map(|index| &objects[index])
        })
        .collect();
        let has_rpath = chain
            .iter()
            .any(|object| !object.rpath.is_empty() || !object.runpath.is_empty());

        let dirs: Vec<PathBuf> = if name.contains('/') {
            Vec::new()
        } else if musl {
            // `LD_LIBRARY_PATH`, then the chain's rpaths, a runpath
            // replacing an object's rpath.
            let chain_rpaths = chain.iter().flat_map(|object| {
                absolute(if object.runpath.is_empty() {
                    &object.rpath
                } else {
                    &object.runpath
                })
            });
            ld_library_path
                .iter()
                .cloned()
                .chain(chain_rpaths)
                .collect()
        } else if chain[0].runpath.is_empty() {
            // glibc: the `DT_RPATH` of the needing object and of the
            // objects above it that have no `DT_RUNPATH`, then
            // `LD_LIBRARY_PATH`.
            let chain_rpaths = chain
                .iter()
                .filter(|object| object.runpath.is_empty())
                .flat_map(|object| absolute(&object.rpath));
            chain_rpaths
                .chain(ld_library_path.iter().cloned())
                .collect()
        } else {
            // glibc: `LD_LIBRARY_PATH`, then only the needing object's
            // own `DT_RUNPATH`.
            ld_library_path
                .iter()
                .cloned()
                .chain(absolute(&chain[0].runpath))
                .collect()
        };

        let direct = if name.contains('/') {
            // A path, used as is: relative ones are relative to the
            // working directory the binary will run in.
            Path::new(&name).is_absolute().then(|| PathBuf::from(&name))
        } else {
            None
        };
        let system_dirs = (!name.contains('/'))
            .then_some(&system.dirs)
            .into_iter()
            .flatten();
        let candidates = direct
            .into_iter()
            .chain(dirs.iter().map(|dir| dir.join(&name)))
            // Looked up lazily: most libraries are found before.
            .chain(
                std::iter::once(())
                    .filter(|()| !name.contains('/'))
                    .flat_map(|()| system.cached(&name)),
            )
            .chain(system_dirs.map(|dir| dir.join(&name)));

        let found = candidates.into_iter().find_map(|path| {
            let lib = elf::parse(&path).ok()??;
            (lib.ident == binary.ident).then_some((path, lib))
        });
        match found {
            Some((path, lib)) => {
                objects.push(Object {
                    rpath: lib.rpath,
                    runpath: lib.runpath,
                    needed_by: Some(needed_by),
                });
                let index = objects.len() - 1;
                queue.extend(lib.needed.into_iter().map(|name| (name, index)));
                libraries.found.insert(name, (path, lib.verdef));
            }
            None => {
                let certain = can_tell_missing
                    && system.complete
                    && !has_rpath
                    && (!name.contains('/') || Path::new(&name).is_absolute());
                if certain {
                    libraries.missing = Some(name);
                    break;
                }
            }
        }
    }

    libraries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flavors() {
        assert_eq!(
            Flavor::of("/lib64/ld-linux-x86-64.so.2"),
            Some(Flavor::Glibc)
        );
        assert_eq!(
            Flavor::of("/lib/ld-linux-aarch64.so.1"),
            Some(Flavor::Glibc)
        );
        assert_eq!(Flavor::of("/lib64/ld64.so.2"), Some(Flavor::Glibc));
        assert_eq!(
            Flavor::of("/lib/ld-musl-x86_64.so.1"),
            Some(Flavor::Musl {
                arch: "x86_64".into()
            })
        );
        assert_eq!(Flavor::of("/system/bin/linker64"), None);
    }

    /// Paths are found in the cache whatever its format, by the
    /// name they end with.
    #[test]
    fn cache_lookup() {
        let path = SystemPath {
            cache: b"glibc-ld.so.cache1.1\0\0libX.so.1\0/opt/cachedir/libX.so.1\0libXY.so.1\0/opt/other/libXY.so.1\0".to_vec(),
            ..SystemPath::default()
        };
        assert_eq!(
            path.cached("libX.so.1").collect::<Vec<_>>(),
            [PathBuf::from("/opt/cachedir/libX.so.1")]
        );
        assert_eq!(path.cached("libZ.so.1").count(), 0);
    }

    /// A library that isn't found is only reported missing when nothing
    /// the search can't follow could have found it.
    #[test]
    fn missing_only_when_certain() {
        let mut binary = elf::parse(&env::current_exe().unwrap()).unwrap().unwrap();
        binary.needed = vec!["libQ-not-installed.so.1".into(), "libc.so".into()];
        binary.rpath.clear();
        binary.runpath.clear();
        let system = SystemPath {
            complete: true,
            ..SystemPath::default()
        };
        let missing = |binary: &Elf, system: &SystemPath, can_tell| {
            libraries(binary, system, can_tell, |name| name == "libc.so").missing
        };

        assert_eq!(
            missing(&binary, &system, true).as_deref(),
            Some("libQ-not-installed.so.1")
        );
        // Not native.
        assert_eq!(missing(&binary, &system, false), None);
        // The configuration couldn't all be read.
        let incomplete = SystemPath::default();
        assert_eq!(missing(&binary, &incomplete, true), None);
        // Any rpath, even an absolute one: glibc searches the binary's
        // `DT_RPATH` for its libraries' dependencies too.
        binary.rpath = vec!["/opt/rp".into()];
        assert_eq!(missing(&binary, &system, true), None);
        binary.rpath.clear();
        binary.runpath = vec!["/opt/rp".into()];
        assert_eq!(missing(&binary, &system, true), None);
    }

    #[test]
    fn musl_path_file() {
        let path = SystemPath::musl("no-such-arch".into());
        assert!(path.complete);
        assert_eq!(
            path.dirs,
            ["/lib", "/usr/local/lib", "/usr/lib"].map(PathBuf::from)
        );
    }
}
