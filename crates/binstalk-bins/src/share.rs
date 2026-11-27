//! Man pages and shell completions shipped in a package, installed under
//! `{ root }/share` next to `{ root }/bin`, where `man` and `bash-completion`
//! find them by themselves when `{ root }/bin` is in `PATH`.
//!
//! The extracted package is walked, and only files that are clearly a man
//! page or a completion script of one of the crate's binaries are picked:
//!
//!  - man pages: `{ bin }.N` and `{ bin }-*.N` (`N` from 1 to 9), optionally
//!    gzipped, go to `man/manN/`.
//!  - bash: `{ bin }.bash` and `{ bin }.bash-completion` go to
//!    `bash-completion/completions/{ bin }`.
//!  - zsh: `_{ bin }` and `*.zsh` files starting with a `#compdef` line that
//!    names a binary go to `zsh/site-functions/_{ bin }`.
//!  - fish: `{ bin }.fish` goes to `fish/vendor_completions.d/{ bin }.fish`.
//!
//! With a single binary, `completion.{bash,fish}` and
//! `completions.{bash,fish}` are taken as its completions too, as clap's
//! examples and skim name them.
//!
//! These are the names `clap_complete` and `clap_mangen` generate, and what
//! e.g. ripgrep, fd, bat and skim ship in their archives.

use std::{
    fs,
    io::{self, BufRead, Read},
    path::{Component, Path, PathBuf},
};

use atomic_file_install::atomic_install_keep_src;
use compact_str::{CompactString, format_compact};

/// Dirs are walked at most this deep below the extracted package, which
/// covers `{ name }-{ target }/man/man1/{ bin }.1`.
const MAX_DEPTH: usize = 4;
/// At most this many files are installed from a package.
const MAX_FILES: usize = 256;
/// Larger files are not man pages or completions.
const MAX_FILE_SIZE: u64 = 4 * 1024 * 1024;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ShareFileKind {
    ManPage,
    BashCompletion,
    ZshCompletion,
    FishCompletion,
}

/// A file of the package to install under the share dir.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareFile {
    pub kind: ShareFileKind,
    /// The extracted file.
    pub source: PathBuf,
    /// Path relative to the share dir, `/` separated, e.g. `man/man1/sk.1`.
    pub dest: CompactString,
}

impl ShareFile {
    /// Path of the installed file.
    pub fn dest_in(&self, share_dir: &Path) -> PathBuf {
        let mut path = share_dir.to_owned();
        path.extend(self.dest.split('/'));
        path
    }

    /// Install the file to [`ShareFile::dest_in`] `share_dir`, replacing
    /// any file already there.
    ///
    /// This is a blocking function, must be called in `block_in_place` mode.
    pub fn install(&self, share_dir: &Path) -> io::Result<()> {
        let dest = self.dest_in(share_dir);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        // The extracted package may be shared with another crate.
        atomic_install_keep_src(&self.source, &dest)
    }
}

/// Split `path`, a path listed by the publisher, into its components, if it
/// is a relative path that stays inside the dir it is joined to.
fn split_relative_path(path: &str) -> Option<Vec<&str>> {
    let components: Vec<&str> = path
        .split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect();
    let is_plain = |c: &&str| {
        *c != ".."
            && !c.contains(['\\', ':', '\0'])
            && matches!(
                Path::new(c).components().collect::<Vec<_>>().as_slice(),
                [Component::Normal(_)]
            )
    };
    (!path.starts_with('/') && !components.is_empty() && components.iter().all(is_plain))
        .then_some(components)
}

/// Path of `dest`, a [`ShareFile::dest`] read back from the manifest, in
/// `share_dir`, if it stays inside of it.
pub fn share_file_path(share_dir: &Path, dest: &str) -> Option<PathBuf> {
    let mut path = share_dir.to_owned();
    path.extend(split_relative_path(dest)?);
    Some(path)
}

/// The man pages and completion scripts of `bins` in `extracted_dir`, the
/// extracted package.
///
/// Dirs are walked up to 4 deep, symlinks are never followed,
/// and at most 256 files are returned. If more than one file maps
/// to the same destination, the first one in path order wins.
pub fn find_share_files(extracted_dir: &Path, bins: &[&str]) -> Vec<ShareFile> {
    let mut files = Vec::new();
    walk(extracted_dir, 0, bins, &mut files);

    files.sort_by(|a, b| a.source.cmp(&b.source));
    let mut seen = std::collections::BTreeSet::new();
    files.retain(|file| seen.insert(file.dest.clone()));
    files.truncate(MAX_FILES);
    files
}

fn walk(path: &Path, depth: usize, bins: &[&str], files: &mut Vec<ShareFile>) {
    if files.len() >= MAX_FILES {
        return;
    }
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };

    if metadata.is_dir() {
        if depth >= MAX_DEPTH {
            return;
        }
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        let mut entries: Vec<_> = entries.filter_map(|e| Some(e.ok()?.path())).collect();
        entries.sort();
        for entry in entries {
            walk(&entry, depth + 1, bins, files);
        }
    } else if metadata.is_file() && metadata.len() <= MAX_FILE_SIZE {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return;
        };
        if let Some((kind, dest)) = classify(name, path, bins) {
            files.push(ShareFile {
                kind,
                source: path.to_owned(),
                dest,
            });
        }
    }
}

/// Whether `stem` is the name of the man page of `bin` or of one of its
/// subcommands, such as `git-perf-add` for `git-perf`.
fn is_man_page_of(stem: &str, bin: &str) -> bool {
    stem.strip_prefix(bin)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('-'))
}

/// The binary that the file `name` is a completion script of, for a shell
/// whose scripts have extension `ext`.
fn completion_of<'a>(name: &str, ext: &str, bins: &[&'a str]) -> Option<&'a str> {
    let stem = name.strip_suffix(ext)?.strip_suffix('.')?;
    if let Some(bin) = bins.iter().find(|bin| **bin == stem) {
        return Some(bin);
    }
    match bins {
        [bin] if stem == "completion" || stem == "completions" => Some(bin),
        _ => None,
    }
}

/// The binary named in the `#compdef` line that the zsh completion function
/// in `path` starts with, preferring `preferred`.
fn compdef_of<'a>(path: &Path, preferred: Option<&str>, bins: &[&'a str]) -> Option<&'a str> {
    let mut line = String::new();
    io::BufReader::new(fs::File::open(path).ok()?)
        .take(4096)
        .read_line(&mut line)
        .ok()?;
    let mut names = line.trim_end().strip_prefix("#compdef")?.split_whitespace();
    let names: Vec<&str> = names.by_ref().collect();

    let is_named = |bin: &&&str| names.contains(*bin);
    preferred
        .and_then(|preferred| bins.iter().filter(is_named).find(|bin| **bin == preferred))
        .or_else(|| bins.iter().find(is_named))
        .copied()
}

fn classify(name: &str, path: &Path, bins: &[&str]) -> Option<(ShareFileKind, CompactString)> {
    // Man page, `sk.1` or `sk.1.gz`
    let unzipped = name.strip_suffix(".gz").unwrap_or(name);
    if let Some((stem, ext)) = unzipped.rsplit_once('.') {
        let mut chars = ext.chars();
        if let Some(section @ '1'..='9') = chars.next()
            && chars.all(|c| c.is_ascii_alphanumeric())
            && bins.iter().any(|bin| is_man_page_of(stem, bin))
        {
            return Some((
                ShareFileKind::ManPage,
                format_compact!("man/man{section}/{name}"),
            ));
        }
    }

    if let Some(stem) = name.strip_prefix('_').or_else(|| name.strip_suffix(".zsh")) {
        let bin = compdef_of(path, Some(stem), bins)?;
        return Some((
            ShareFileKind::ZshCompletion,
            format_compact!("zsh/site-functions/_{bin}"),
        ));
    }

    if let Some(bin) =
        completion_of(name, "bash", bins).or_else(|| completion_of(name, "bash-completion", bins))
    {
        return Some((
            ShareFileKind::BashCompletion,
            format_compact!("bash-completion/completions/{bin}"),
        ));
    }

    if let Some(bin) = completion_of(name, "fish", bins) {
        return Some((
            ShareFileKind::FishCompletion,
            format_compact!("fish/vendor_completions.d/{bin}.fish"),
        ));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, path: &str, contents: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn dests(files: &[ShareFile]) -> Vec<(ShareFileKind, &str)> {
        files.iter().map(|f| (f.kind, f.dest.as_str())).collect()
    }

    #[test]
    fn relative_paths() {
        assert_eq!(split_relative_path("man"), Some(vec!["man"]));
        assert_eq!(split_relative_path("./a//b/"), Some(vec!["a", "b"]));
        assert_eq!(split_relative_path(""), None);
        assert_eq!(split_relative_path("."), None);
        assert_eq!(split_relative_path("/etc"), None);
        assert_eq!(split_relative_path("a/../../b"), None);
        assert_eq!(split_relative_path("a\\b"), None);
        assert_eq!(split_relative_path("C:x"), None);

        let share = Path::new("/share");
        assert_eq!(
            share_file_path(share, "man/man1/rg.1"),
            Some(PathBuf::from("/share/man/man1/rg.1"))
        );
        assert_eq!(share_file_path(share, "../bin/rg"), None);
    }

    /// Laid out as skim 5.9.0's archive.
    #[test]
    fn skim() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let pkg = "skim-x86_64-unknown-linux-gnu";
        for (path, contents) in [
            ("sk", ""),
            ("README.md", ""),
            ("man/man1/sk.1", ".TH sk 1"),
            ("man/man1/sk-tmux.1", ".TH sk-tmux 1"),
            (
                "shell/completion.zsh",
                "#compdef sk\n\nautoload -U is-at-least\n",
            ),
            ("shell/completion.bash", "_sk() {\n"),
            ("shell/completion.fish", "complete -c sk\n"),
            ("shell/completion.nu", ""),
            ("shell/key-bindings.zsh", "# key bindings\n"),
            ("shell/key-bindings.bash", ""),
            ("shell/key-bindings.fish", ""),
            ("shell/README.md", ""),
        ] {
            write(root, &format!("{pkg}/{path}"), contents);
        }

        let files = find_share_files(root, &["sk"]);
        assert_eq!(
            dests(&files),
            [
                (ShareFileKind::ManPage, "man/man1/sk-tmux.1"),
                (ShareFileKind::ManPage, "man/man1/sk.1"),
                (
                    ShareFileKind::BashCompletion,
                    "bash-completion/completions/sk"
                ),
                (
                    ShareFileKind::FishCompletion,
                    "fish/vendor_completions.d/sk.fish"
                ),
                (ShareFileKind::ZshCompletion, "zsh/site-functions/_sk"),
            ]
        );
        assert_eq!(files[1].source, root.join(pkg).join("man/man1/sk.1"));
    }

    /// Laid out as ripgrep 14.1.1's archive.
    #[test]
    fn ripgrep() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (path, contents) in [
            ("rg", ""),
            ("doc/rg.1", ""),
            ("doc/GUIDE.md", ""),
            ("complete/_rg", "#compdef rg\n"),
            ("complete/_rg.ps1", ""),
            ("complete/rg.bash", ""),
            ("complete/rg.fish", ""),
        ] {
            write(
                root,
                &format!("ripgrep-14.1.1-x86_64-unknown-linux-musl/{path}"),
                contents,
            );
        }

        assert_eq!(
            dests(&find_share_files(root, &["rg"])),
            [
                (ShareFileKind::ZshCompletion, "zsh/site-functions/_rg"),
                (
                    ShareFileKind::BashCompletion,
                    "bash-completion/completions/rg"
                ),
                (
                    ShareFileKind::FishCompletion,
                    "fish/vendor_completions.d/rg.fish"
                ),
                (ShareFileKind::ManPage, "man/man1/rg.1"),
            ]
        );
    }

    /// Laid out as git-workon 0.31.0's archive, generated by clap, with
    /// files of another command.
    #[test]
    fn clap_names() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (path, contents) in [
            ("git-workon.1", ""),
            ("git-workon.bash", ""),
            ("git-workon.fish", ""),
            ("_git-workon", "#compdef git-workon\n"),
            ("other.1", ""),
            ("other.bash", ""),
            ("_other", "#compdef other\n"),
        ] {
            write(root, path, contents);
        }

        let files = find_share_files(root, &["git-workon", "gw"]);
        assert_eq!(
            dests(&files),
            [
                (
                    ShareFileKind::ZshCompletion,
                    "zsh/site-functions/_git-workon"
                ),
                (ShareFileKind::ManPage, "man/man1/git-workon.1"),
                (
                    ShareFileKind::BashCompletion,
                    "bash-completion/completions/git-workon"
                ),
                (
                    ShareFileKind::FishCompletion,
                    "fish/vendor_completions.d/git-workon.fish"
                ),
            ]
        );
    }

    #[test]
    fn generic_names_need_a_single_bin() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "completions.bash", "");
        write(root, "completion.zsh", "#compdef b\n");

        assert_eq!(
            dests(&find_share_files(root, &["a", "b"])),
            [(ShareFileKind::ZshCompletion, "zsh/site-functions/_b")]
        );
        assert_eq!(
            dests(&find_share_files(root, &["b"])),
            [
                (ShareFileKind::ZshCompletion, "zsh/site-functions/_b"),
                (
                    ShareFileKind::BashCompletion,
                    "bash-completion/completions/b"
                ),
            ]
        );
    }

    #[test]
    fn man_page_names() {
        let path = Path::new("unused");
        let bins = ["rg"];
        let man = |name| classify(name, path, &bins).map(|(_, dest)| dest);
        assert_eq!(man("rg.1").as_deref(), Some("man/man1/rg.1"));
        assert_eq!(man("rg.1.gz").as_deref(), Some("man/man1/rg.1.gz"));
        assert_eq!(man("rg-config.5").as_deref(), Some("man/man5/rg-config.5"));
        assert_eq!(man("rg.3pm").as_deref(), Some("man/man3/rg.3pm"));
        assert_eq!(man("rga.1"), None);
        assert_eq!(man("ls.1"), None);
        assert_eq!(man("rg.0"), None);
        assert_eq!(man("rg.md"), None);
        assert_eq!(man("rg.bash.gz"), None);
    }

    #[test]
    fn zsh_needs_compdef() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "_rg", "# not a completion function\n");
        write(root, "rg.zsh", "#compdef ls\n");
        assert_eq!(dests(&find_share_files(root, &["rg"])), []);
    }

    #[test]
    fn depth_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "a/b/c/rg.1", "");
        write(root, "a/b/c/d/rg.bash", "");
        assert_eq!(
            dests(&find_share_files(root, &["rg"])),
            [(ShareFileKind::ManPage, "man/man1/rg.1")]
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed() {
        let outside = tempfile::tempdir().unwrap();
        write(outside.path(), "man1/rg.1", "");
        write(outside.path(), "rg.bash", "");

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::os::unix::fs::symlink(outside.path(), root.join("man")).unwrap();
        fs::create_dir(root.join("doc")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("rg.bash"), root.join("doc/rg.bash"))
            .unwrap();

        assert_eq!(dests(&find_share_files(root, &["rg"])), []);
    }

    #[test]
    fn install() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("pkg");
        write(&root, "doc/rg.1", "page");
        let share_dir = dir.path().join("share");

        let files = find_share_files(&root, &["rg"]);
        assert_eq!(files.len(), 1);
        files[0].install(&share_dir).unwrap();
        assert_eq!(
            fs::read_to_string(share_dir.join("man/man1/rg.1")).unwrap(),
            "page"
        );
        // The extracted file is kept, as the package may be shared.
        assert!(root.join("doc/rg.1").exists());
    }
}
