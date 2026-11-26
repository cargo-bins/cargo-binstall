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
    // Default tags of workspace release tools such as release-plz and cargo-release.
    template!("{ repo }/releases/download/{ name }-v{ version }"),
    template!("{ repo }/releases/download/{ name }-{ version }"),
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

/// The release tags that dist's `{tag}` in `simple-download-url` is tried
/// as: those of [`GITHUB_RELEASE_PATHS`] that need no repository or
/// subcrate.
const DIST_SIMPLE_TAGS: &[Template<'_>] = &[
    template!("{ version }"),
    template!("v{ version }"),
    template!("{ name }-v{ version }"),
    template!("{ name }-{ version }"),
];

/// The urls the `dist-manifest.json` of a release may be at, if dist's
/// `simple-download-url` is `simple_download_url`, e.g.
/// `https://static.myapp.com/{tag}`: the directory the release's files
/// are uploaded to, with `{tag}` for its git tag.
///
/// See <https://axodotdev.github.io/cargo-dist/book/reference/config.html#simple-download-url>.
pub(crate) fn dist_simple_manifest_url_templates(
    simple_download_url: &str,
) -> impl Iterator<Item = Template<'_>> {
    let dir = simple_download_url.trim_end_matches('/');
    // Without `{tag}`, every tag has the same url.
    let tags = if dir.contains("{tag}") {
        DIST_SIMPLE_TAGS
    } else {
        &DIST_SIMPLE_TAGS[..1]
    };
    tags.iter().map(move |tag| {
        let mut template = Template::new(&[], None);
        for (i, text) in dir.split("{tag}").enumerate() {
            if i > 0 {
                template += tag;
            }
            if !text.is_empty() {
                template += Item::Text(text);
            }
        }
        template += Item::Text("/dist-manifest.json");
        template
    })
}

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

    /// The urls the `dist-manifest.json` uploaded by
    /// [dist](https://github.com/axodotdev/cargo-dist) may be at, one for each
    /// release path, if dist supports this host.
    pub(crate) fn dist_manifest_url_templates(
        self,
    ) -> Option<impl Iterator<Item = Template<'static>> + Clone + 'static> {
        (self == RepositoryHost::GitHub).then(|| {
            GITHUB_RELEASE_PATHS.iter().map(|path| {
                let mut template = path.clone();
                template += Item::Text("/dist-manifest.json");
                template
            })
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

    #[test]
    fn dist_simple_manifest_urls() {
        let urls = |url| {
            dist_simple_manifest_url_templates(url)
                .map(|template| render(&template))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            urls("https://static.example.com/{tag}/"),
            [
                "https://static.example.com/{ version }/dist-manifest.json",
                "https://static.example.com/v{ version }/dist-manifest.json",
                "https://static.example.com/{ name }-v{ version }/dist-manifest.json",
                "https://static.example.com/{ name }-{ version }/dist-manifest.json",
            ]
        );
        assert_eq!(
            urls("https://example.com/releases/{tag}/files"),
            [
                "https://example.com/releases/{ version }/files/dist-manifest.json",
                "https://example.com/releases/v{ version }/files/dist-manifest.json",
                "https://example.com/releases/{ name }-v{ version }/files/dist-manifest.json",
                "https://example.com/releases/{ name }-{ version }/files/dist-manifest.json",
            ]
        );
        assert_eq!(
            urls("https://example.com/latest"),
            ["https://example.com/latest/dist-manifest.json"]
        );
    }

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
