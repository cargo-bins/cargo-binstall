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

/// glibc's configuration of where to look.
const LD_SO_CONF: &str = "/etc/ld.so.conf";
/// glibc's cache of what `ldconfig` found, including directories given
/// on its command line rather than in [`LD_SO_CONF`].
const LD_SO_CACHE: &str = "/etc/ld.so.cache";
/// musl's default path, when `/etc/ld-musl-$ARCH.path` doesn't exist.
const MUSL_DEFAULT_PATH: &str = "/lib:/usr/local/lib:/usr/lib";
/// Stop following dependencies after this many libraries: a real
/// program needs far fewer, so a longer chain is a malformed or hostile
/// one.
const MAX_LIBRARIES: usize = 1024;
/// How deep `include` directives in [`LD_SO_CONF`] are followed.
const MAX_INCLUDE_DEPTH: usize = 8;

/// The loader the binary asks for, by how it looks up libraries.
#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum Flavor {
    Glibc,
    /// musl, with the `$ARCH` from its loader name `ld-musl-$ARCH.so.1`.
    Musl {
        arch: String,
    },
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

/// The directories the loader searches after the binary's own rpaths
/// and `LD_LIBRARY_PATH`, and whether that list is complete.
#[derive(Debug, Default)]
pub(crate) struct SystemPath {
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
            Flavor::Musl { arch } => Self::musl(arch),
        }
    }

    fn glibc() -> Self {
        let mut path = Self {
            cache: fs::read(LD_SO_CACHE).unwrap_or_default(),
            complete: true,
            ..Self::default()
        };
        // Without `ld.so.conf`, glibc may be configured another way, e.g.
        // NixOS' nix-ld, which takes its path from the environment.
        path.complete &= parse_ld_so_conf(Path::new(LD_SO_CONF), 0, &mut path.dirs);

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

    fn musl(arch: &str) -> Self {
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

/// Add the directories `ld.so.conf` at `path` lists to `dirs`, following
/// its `include`s, and return whether all of it could be read.
///
/// The syntax is `ldconfig`'s: `#` starts a comment, surrounding
/// whitespace is ignored, `include` takes glob patterns relative to the
/// including file, `hwcap` lines are obsolete and ignored, and anything
/// else is a directory.
fn parse_ld_so_conf(path: &Path, depth: usize, dirs: &mut Vec<PathBuf>) -> bool {
    let Ok(conf) = fs::read_to_string(path) else {
        return false;
    };
    let mut complete = true;
    for line in conf.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let mut words = line.split_ascii_whitespace();
        match words.next() {
            Some("include") => {
                for pattern in words {
                    if depth >= MAX_INCLUDE_DEPTH {
                        complete = false;
                        break;
                    }
                    let pattern = match path.parent() {
                        Some(parent) => parent.join(pattern),
                        None => PathBuf::from(pattern),
                    };
                    match glob(&pattern) {
                        Some(files) => {
                            for file in files {
                                complete &= parse_ld_so_conf(&file, depth + 1, dirs);
                            }
                        }
                        None => complete = false,
                    }
                }
            }
            Some("hwcap") => {}
            // Old versions of ldconfig also split a line on ':', ',' and
            // whitespace, and allowed a `=TYPE` suffix.
            _ => dirs.extend(
                line.split([':', ',', ' ', '\t'])
                    .map(|dir| dir.split('=').next().unwrap_or_default())
                    .filter(|dir| dir.starts_with('/'))
                    .map(PathBuf::from),
            ),
        }
    }
    complete
}

/// The files matching `pattern`, sorted, whose wildcards (`*` and `?`)
/// may only be in its last component, as in `ld.so.conf.d/*.conf`. `None`
/// if it can't be expanded.
fn glob(pattern: &Path) -> Option<Vec<PathBuf>> {
    let name = pattern.file_name()?.to_str()?;
    let dir = pattern.parent()?;
    if dir.to_str()?.contains(['*', '?', '[']) || name.contains('[') {
        return None;
    }
    if !name.contains(['*', '?']) {
        return Some(vec![pattern.to_owned()]);
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        // A pattern matching nothing is fine.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Some(Vec::new()),
        Err(_) => return None,
    };
    let mut files: Vec<_> = entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|file| !file.starts_with('.') && wildcard_match(name, file))
        })
        .map(|entry| entry.path())
        .collect();
    files.sort();
    Some(files)
}

/// Whether `name` matches `pattern`, whose `*` matches any run of
/// characters and `?` any one.
fn wildcard_match(pattern: &str, name: &str) -> bool {
    let (pattern, name): (Vec<char>, Vec<char>) =
        (pattern.chars().collect(), name.chars().collect());
    let (mut p, mut n) = (0, 0);
    let mut star = None;
    while n < name.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == name[n]) {
            p += 1;
            n += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, n));
            p += 1;
        } else if let Some((star_p, star_n)) = star {
            p = star_p + 1;
            n = star_n + 1;
            star = Some((star_p, star_n + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

/// What the search found.
#[derive(Debug, Default)]
pub(crate) struct Libraries {
    /// The libraries found, by the name they were needed as.
    pub(crate) found: HashMap<String, PathBuf>,
    /// A library the loader certainly can't find, if any.
    pub(crate) missing: Option<String>,
}

/// A loaded object: the binary, or a library found for it.
struct Object {
    /// Its rpath and runpath entries.
    rpaths: Vec<String>,
    /// The object that needed it, `None` for the binary.
    needed_by: Option<usize>,
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

    let mut libraries = Libraries::default();
    let mut objects = vec![Object {
        rpaths: binary.rpaths.clone(),
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

        // The rpaths of the object needing it, then of the one that
        // needed that, up to the binary.
        let mut rpaths = Vec::new();
        let mut has_rpath = false;
        let mut object = Some(needed_by);
        while let Some(index) = object {
            has_rpath |= !objects[index].rpaths.is_empty();
            rpaths.extend(
                objects[index]
                    .rpaths
                    .iter()
                    .flat_map(|rpath| rpath.split(':'))
                    // `$ORIGIN` and relative entries depend on where the
                    // binary ends up, not where it is now.
                    .filter(|dir| dir.starts_with('/') && !dir.contains('$'))
                    .map(PathBuf::from),
            );
            object = objects[index].needed_by;
        }
        let candidates: Box<dyn Iterator<Item = PathBuf>> = if name.contains('/') {
            // A path, used as is: relative ones are relative to the
            // working directory the binary will run in.
            Box::new(
                Path::new(&name)
                    .is_absolute()
                    .then(|| PathBuf::from(&name))
                    .into_iter(),
            )
        } else {
            Box::new(
                rpaths
                    .iter()
                    .chain(&ld_library_path)
                    .map(|dir| dir.join(&name))
                    .chain(system.cached(&name))
                    .chain(system.dirs.iter().map(|dir| dir.join(&name))),
            )
        };

        let found = candidates.into_iter().find_map(|path| {
            let lib = elf::parse(&path).ok()??;
            (lib.ident == binary.ident).then_some((path, lib))
        });
        match found {
            Some((path, lib)) => {
                objects.push(Object {
                    rpaths: lib.rpaths,
                    needed_by: Some(needed_by),
                });
                let index = objects.len() - 1;
                queue.extend(lib.needed.into_iter().map(|name| (name, index)));
                libraries.found.insert(name, path);
            }
            None => {
                let certain = can_tell_missing
                    && system.complete
                    && !has_rpath
                    && (!name.contains('/') || Path::new(&name).is_absolute());
                if certain && libraries.missing.is_none() {
                    libraries.missing = Some(name);
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

    #[test]
    fn wildcards() {
        assert!(wildcard_match("*.conf", "libc.conf"));
        assert!(wildcard_match("*.conf", ".conf"));
        assert!(!wildcard_match("*.conf", "libc.conf.bak"));
        assert!(wildcard_match("x86_64-*.conf", "x86_64-linux-gnu.conf"));
        assert!(wildcard_match("a?c", "abc"));
        assert!(!wildcard_match("a?c", "ac"));
        assert!(wildcard_match("*", "anything"));
    }

    /// `ld.so.conf` is parsed the way `ldconfig` does: comments,
    /// surrounding whitespace, blank lines, and `include` globs relative
    /// to the including file.
    #[test]
    fn ld_so_conf_syntax() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        fs::create_dir(d.join("conf.d")).unwrap();
        fs::write(
            d.join("ld.so.conf"),
            "# comment\n\n   \ninclude conf.d/*.conf\n/opt/top\n",
        )
        .unwrap();
        fs::write(d.join("conf.d/a.conf"), "  /opt/fooA  \n").unwrap();
        fs::write(d.join("conf.d/b.conf"), "/opt/fooB   # vendor libs\n").unwrap();
        fs::write(d.join("conf.d/c.conf.bak"), "/opt/ignored\n").unwrap();
        fs::write(
            d.join("conf.d/d.conf"),
            "/opt/d1:/opt/d2,/opt/d3=libc6\nhwcap 0 nosegneg\n",
        )
        .unwrap();

        let mut dirs = Vec::new();
        assert!(parse_ld_so_conf(&d.join("ld.so.conf"), 0, &mut dirs));
        assert_eq!(
            dirs,
            [
                "/opt/fooA",
                "/opt/fooB",
                "/opt/d1",
                "/opt/d2",
                "/opt/d3",
                "/opt/top"
            ]
            .map(PathBuf::from)
        );

        // A file that includes itself is followed only so far, which
        // makes the configuration incomplete.
        fs::write(d.join("loop.conf"), "include loop.conf\n").unwrap();
        assert!(!parse_ld_so_conf(&d.join("loop.conf"), 0, &mut Vec::new()));
        // A missing configuration is incomplete.
        assert!(!parse_ld_so_conf(
            &d.join("missing.conf"),
            0,
            &mut Vec::new()
        ));
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
        binary.rpaths.clear();
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
        binary.rpaths = vec!["/opt/rp".into()];
        assert_eq!(missing(&binary, &system, true), None);
    }

    #[test]
    fn musl_path_file() {
        let path = SystemPath::musl("no-such-arch");
        assert!(path.complete);
        assert_eq!(
            path.dirs,
            ["/lib", "/usr/local/lib", "/usr/lib"].map(PathBuf::from)
        );
    }
}
