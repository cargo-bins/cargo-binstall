use itertools::Itertools;
use leon::{Item, Template};
use leon_macros::template;
use url::Url;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RepositoryHost {
    GitHub,
    GitLab,
    BitBucket,
    SourceForge,
    Codeberg,
    Unknown,
}

/// Make sure to update `binstalk_bins::possible_bin_dirs`
/// if you modified FULL_FILENAMES or NOVERSION_FILENAMES.
pub const FULL_FILENAMES: &[Template<'_>] = &[
    template!("/{ name }-{ target }-v{ version }{ archive-suffix }"),
    template!("/{ name }-{ target }-{ version }{ archive-suffix }"),
    template!("/{ name }-{ version }-{ target }{ archive-suffix }"),
    template!("/{ name }-v{ version }-{ target }{ archive-suffix }"),
    template!("/{ name }_{ target }_v{ version }{ archive-suffix }"),
    template!("/{ name }_{ target }_{ version }{ archive-suffix }"),
    template!("/{ name }_{ version }_{ target }{ archive-suffix }"),
    template!("/{ name }_v{ version }_{ target }{ archive-suffix }"),
];

pub const NOVERSION_FILENAMES: &[Template<'_>] = &[
    template!("/{ name }-{ target }{ archive-suffix }"),
    template!("/{ name }_{ target }{ archive-suffix }"),
];

const GITHUB_RELEASE_PATHS: &[Template<'_>] = &[
    template!("{ repo }/releases/download/{ version }"),
    template!("{ repo }/releases/download/v{ version }"),
    // %2F is escaped form of '/'
    template!("{ repo }/releases/download/{ subcrate }%2F{ version }"),
    template!("{ repo }/releases/download/{ subcrate }%2Fv{ version }"),
];

const GITLAB_RELEASE_PATHS: &[Template<'_>] = &[
    template!("{ repo }/-/releases/{ version }/downloads/binaries"),
    template!("{ repo }/-/releases/v{ version }/downloads/binaries"),
    // %2F is escaped form of '/'
    template!("{ repo }/-/releases/{ subcrate }%2F{ version }/downloads/binaries"),
    template!("{ repo }/-/releases/{ subcrate }%2Fv{ version }/downloads/binaries"),
];

const BITBUCKET_RELEASE_PATHS: &[Template<'_>] = &[template!("{ repo }/downloads")];

// No `{ subcrate }` paths: `RepoInfo::detect_subcrate` never finds a subcrate
// in a SourceForge URL, so they would always be filtered out.
const SOURCEFORGE_RELEASE_PATHS: &[Template<'_>] = &[
    template!("{ repo }/files/binaries/{ version }"),
    template!("{ repo }/files/binaries/v{ version }"),
];

/// Every filename is appended to every release path, followed by `suffix`.
struct DefaultPkgUrls {
    paths: &'static [Template<'static>],
    filenames: &'static [&'static [Template<'static>]],
    suffix: &'static str,
}

impl RepositoryHost {
    pub fn guess_git_hosting_services(repo: &Url) -> Self {
        use RepositoryHost::*;

        match repo.domain() {
            Some(domain) if domain.starts_with("github") => GitHub,
            Some(domain) if domain.starts_with("gitlab") => GitLab,
            Some("bitbucket.org") => BitBucket,
            Some("sourceforge.net") => SourceForge,
            Some("codeberg.org") => Codeberg,
            _ => Unknown,
        }
    }

    /// The parts that make up the default `pkg-url` templates of this host.
    ///
    /// SUPPORT.md documents these, see `tests::support_md`.
    fn default_pkg_urls(self) -> Option<DefaultPkgUrls> {
        use RepositoryHost::*;

        Some(match self {
            GitHub => DefaultPkgUrls {
                paths: GITHUB_RELEASE_PATHS,
                filenames: &[FULL_FILENAMES, NOVERSION_FILENAMES],
                suffix: "",
            },
            GitLab => DefaultPkgUrls {
                paths: GITLAB_RELEASE_PATHS,
                filenames: &[FULL_FILENAMES, NOVERSION_FILENAMES],
                suffix: "",
            },
            BitBucket => DefaultPkgUrls {
                paths: BITBUCKET_RELEASE_PATHS,
                filenames: &[FULL_FILENAMES],
                suffix: "",
            },
            SourceForge => DefaultPkgUrls {
                paths: SOURCEFORGE_RELEASE_PATHS,
                filenames: &[FULL_FILENAMES, NOVERSION_FILENAMES],
                suffix: "/download",
            },
            // Codeberg (Forgejo) has the same release paths as GitHub.
            Codeberg => DefaultPkgUrls {
                paths: GITHUB_RELEASE_PATHS,
                filenames: &[FULL_FILENAMES, NOVERSION_FILENAMES],
                suffix: "",
            },
            Unknown => return None,
        })
    }

    pub fn get_default_pkg_url_template(
        self,
    ) -> Option<impl Iterator<Item = Template<'static>> + Clone + 'static> {
        let DefaultPkgUrls {
            paths,
            filenames,
            suffix,
        } = self.default_pkg_urls()?;
        Some(apply_filenames_to_paths(paths, filenames, suffix))
    }
}

fn apply_filenames_to_paths(
    paths: &'static [Template<'static>],
    filenames: &'static [&'static [Template<'static>]],
    suffix: &'static str,
) -> impl Iterator<Item = Template<'static>> + Clone + 'static {
    filenames
        .iter()
        .flat_map(|fs| fs.iter())
        .cartesian_product(paths.iter())
        .map(move |(filename, path)| {
            let mut template = path.clone() + filename;
            template += Item::Text(suffix);
            template
        })
}

#[cfg(test)]
mod tests {
    use std::{env, fs, path::Path};

    use super::*;

    /// Renders `template` back into the syntax used in `Cargo.toml`.
    fn render(template: &Template<'_>) -> String {
        template
            .items
            .iter()
            .map(|item| match item {
                Item::Text(text) => text
                    .replace('\\', "\\\\")
                    .replace('{', "\\{")
                    .replace('}', "\\}"),
                Item::Key(key) => format!("{{ {key} }}"),
            })
            .collect()
    }

    /// Replaces the lines between `<!-- BEGIN generated:NAME ... -->` and
    /// `<!-- END generated:NAME -->` in the workspace document `doc`.
    ///
    /// Rewrites the sections that are out of date, then fails so that the
    /// change gets noticed, unless the `UPDATE_DOCS` env var is set (see
    /// `just update-docs`).
    fn sync_doc_sections(doc: &str, sections: &[(&str, String)]) {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        // The packaged crate (crates.io, vendored sources) doesn't ship
        // the workspace docs, so only check inside the workspace.
        if manifest_dir.parent().and_then(Path::file_name) != Some("crates".as_ref()) {
            return;
        }
        let path = manifest_dir.join("../..").join(doc);

        let original = fs::read_to_string(&path).unwrap().replace("\r\n", "\n");
        let mut updated = original.clone();
        for (name, content) in sections {
            let begin = format!("<!-- BEGIN generated:{name} ");
            let end = format!("<!-- END generated:{name} -->");

            let start = updated
                .find(&begin)
                .unwrap_or_else(|| panic!("{doc}: missing `{begin}... -->`"));
            let start = start + updated[start..].find('\n').unwrap() + 1;
            let len = updated[start..]
                .find(&end)
                .unwrap_or_else(|| panic!("{doc}: missing `{end}`"));
            updated.replace_range(start..start + len, content);
        }

        if updated != original {
            fs::write(&path, updated).unwrap();
            assert!(
                env::var_os("UPDATE_DOCS").is_some(),
                "{doc} was out of date and has been regenerated, commit the changes"
            );
        }
    }

    #[test]
    fn support_md() {
        let mut filenames = String::new();
        for (filenames_set, note) in [
            (FULL_FILENAMES, ""),
            (NOVERSION_FILENAMES, r#" ("versionless")"#),
        ] {
            for filename in filenames_set {
                let filename = render(filename);
                let filename = filename.strip_prefix('/').unwrap();
                filenames += &format!("- `{filename}`{note}\n");
            }
        }

        let bin_dirs = binstalk_bins::possible_bin_dirs("{ name }", "{ target }", "{ version }")
            .map(|dir| format!("- `{dir}`\n"))
            .collect();

        let mut sections = vec![
            ("default-bin-dirs", bin_dirs),
            ("pkg-url-filenames", filenames),
        ];

        for (host, name) in [
            (RepositoryHost::GitHub, "pkg-url-paths-github"),
            (RepositoryHost::GitLab, "pkg-url-paths-gitlab"),
            (RepositoryHost::BitBucket, "pkg-url-paths-bitbucket"),
            (RepositoryHost::SourceForge, "pkg-url-paths-sourceforge"),
            (RepositoryHost::Codeberg, "pkg-url-paths-codeberg"),
        ] {
            let paths = host
                .default_pkg_urls()
                .unwrap()
                .paths
                .iter()
                .map(|path| format!("- `{}/`\n", render(path)))
                .collect();
            sections.push((name, paths));
        }

        sync_doc_sections("SUPPORT.md", &sections);
    }
}
