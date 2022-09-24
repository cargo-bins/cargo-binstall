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

pub const FULL_FILENAMES: &[&str] = &[
    "{ name }-{ target }-v{ version }.{ archive-format }",
    "{ name }-{ target }-{ version }.{ archive-format }",
    "{ name }-{ version }-{ target }.{ archive-format }",
    "{ name }-v{ version }-{ target }.{ archive-format }",
];

pub const NOVERSION_FILENAMES: &[&str] = &["{ name }-{ target }.{ archive-format }"];

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

    pub fn get_default_pkg_url_template(self) -> Option<Vec<String>> {
        use RepositoryHost::*;

        match self {
            GitHub => Some(apply_filenames_to_paths(
                &[
                    "{ repo }/releases/download/{ version }",
                    "{ repo }/releases/download/v{ version }",
                ],
                &[FULL_FILENAMES, NOVERSION_FILENAMES],
            )),
            GitLab => Some(apply_filenames_to_paths(
                &[
                    "{ repo }/-/releases/{ version }/downloads/binaries",
                    "{ repo }/-/releases/v{ version }/downloads/binaries",
                ],
                &[FULL_FILENAMES, NOVERSION_FILENAMES],
            )),
            BitBucket => Some(apply_filenames_to_paths(
                &["{ repo }/downloads"],
                &[FULL_FILENAMES],
            )),
            SourceForge => Some(
                apply_filenames_to_paths(
                    &[
                        "{ repo }/files/binaries/{ version }",
                        "{ repo }/files/binaries/v{ version }",
                    ],
                    &[FULL_FILENAMES, NOVERSION_FILENAMES],
                )
                .into_iter()
                .map(|url| format!("{url}/download"))
                .collect(),
            ),
            Unknown => None,
        }
    }
}

fn apply_filenames_to_paths(paths: &[&str], filenames: &[&[&str]]) -> Vec<String> {
    filenames
        .iter()
        .flat_map(|fs| fs.iter())
        .flat_map(|filename| paths.iter().map(move |path| format!("{path}/{filename}")))
        .collect()
}
