use itertools::Itertools;
use leon::{template2, Item, Template};
use url::Url;

use crate::errors::BinstallError;

#[derive(Copy, Clone, Debug)]
pub enum RepositoryHost {
    GitHub,
    GitLab,
    BitBucket,
    SourceForge,
    Unknown,
}

/// Make sure to update possible_dirs in `bins::infer_bin_dir_template`
/// if you modified FULL_FILENAMES or NOVERSION_FILENAMES.
pub const FULL_FILENAMES: &[Template<'_>] = &[
    template2!({ "name" }, "-", { "target" }, "-v", { "version" }, {
        "archive-suffix"
    }),
    template2!({ "name" }, "-", { "target" }, "-", { "version" }, {
        "archive-suffix"
    }),
    template2!({ "name" }, "-", { "version" }, "-", { "target" }, {
        "archive-suffix"
    }),
    template2!({ "name" }, "-v", { "version" }, "-", { "target" }, {
        "archive-suffix"
    }),
    template2!({ "name" }, "_", { "target" }, "_v", { "version" }, {
        "archive-suffix"
    }),
    template2!({ "name" }, "_", { "target" }, "_", { "version" }, {
        "archive-suffix"
    }),
    template2!({ "name" }, "_", { "version" }, "_", { "target" }, {
        "archive-suffix"
    }),
    template2!({ "name" }, "_v", { "version" }, "_", { "target" }, {
        "archive-suffix"
    }),
];

pub const NOVERSION_FILENAMES: &[Template<'_>] = &[
    template2!({ "name" }, "-", { "target" }, { "archive-suffix" }),
    template2!({ "name" }, "_", { "target" }, { "archive-suffix" }),
];

const GITHUB_RELEASE_PATHS: &[Template<'_>] = &[
    template2!({ "repo" }, "/releases/download/", { "version" }),
    template2!({ "repo" }, "/releases/download/v", { "version" }),
];

const GITLAB_RELEASE_PATHS: &[Template<'_>] = &[
    template2!(
        { "repo" },
        "/-/releases/",
        { "version" },
        "/downloads/binaries"
    ),
    template2!(
        { "repo" },
        "/-/releases/v",
        { "version" },
        "/downloads/binaries"
    ),
];

const BITBUCKET_RELEASE_PATHS: &[Template<'_>] = &[template2!({ "repo" }, "/downloads")];

const SOURCEFORGE_RELEASEW_PATHS: &[Template<'_>] = &[
    template2!({ "repo" }, "/files/binaries/", { "version" }),
    template2!({ "repo" }, "/files/binaries/v", { "version" }),
];

impl RepositoryHost {
    pub fn guess_git_hosting_services(repo: &Url) -> Result<Self, BinstallError> {
        use RepositoryHost::*;

        match repo.domain() {
            Some(domain) if domain.starts_with("github") => Ok(GitHub),
            Some(domain) if domain.starts_with("gitlab") => Ok(GitLab),
            Some(domain) if domain == "bitbucket.org" => Ok(BitBucket),
            Some(domain) if domain == "sourceforge.net" => Ok(SourceForge),
            _ => Ok(Unknown),
        }
    }

    pub fn get_default_pkg_url_template(
        self,
    ) -> Option<impl Iterator<Item = Template<'static>> + Clone + 'static> {
        use RepositoryHost::*;

        match self {
            GitHub => Some(apply_filenames_to_paths(
                GITHUB_RELEASE_PATHS,
                &[FULL_FILENAMES, NOVERSION_FILENAMES],
                "",
            )),
            GitLab => Some(apply_filenames_to_paths(
                GITLAB_RELEASE_PATHS,
                &[FULL_FILENAMES, NOVERSION_FILENAMES],
                "",
            )),
            BitBucket => Some(apply_filenames_to_paths(
                BITBUCKET_RELEASE_PATHS,
                &[FULL_FILENAMES],
                "",
            )),
            SourceForge => Some(apply_filenames_to_paths(
                SOURCEFORGE_RELEASEW_PATHS,
                &[FULL_FILENAMES, NOVERSION_FILENAMES],
                "/download",
            )),
            Unknown => None,
        }
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
            let mut template = path.clone() + filename.clone();
            template.items.to_mut().push(Item::Text(suffix));
            template
        })
}
