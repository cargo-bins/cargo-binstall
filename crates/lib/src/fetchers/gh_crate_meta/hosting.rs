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

        let full_filenames = &[
            "{ name }-{ target }-v{ version }.{ archive-format }",
            "{ name }-{ target }-{ version }.{ archive-format }",
            "{ name }-{ version }-{ target }.{ archive-format }",
            "{ name }-v{ version }-{ target }.{ archive-format }",
            "{ name }-{ version }-{ target }.{ archive-format }",
            "{ name }-v{ version }-{ target }.{ archive-format }",
        ];

        let noversion_filenames = &["{ name }-{ target }.{ archive-format }"];

        match self {
            GitHub => Some(apply_filenames_to_paths(
                &[
                    "{ repo }/releases/download/{ version }",
                    "{ repo }/releases/download/v{ version }",
                ],
                &[full_filenames, noversion_filenames],
            )),
            GitLab => Some(apply_filenames_to_paths(
                &[
                    "{ repo }/-/releases/{ version }/downloads/binaries/",
                    "{ repo }/-/releases/v{ version }/downloads/binaries/",
                ],
                &[full_filenames, noversion_filenames],
            )),
            BitBucket => Some(apply_filenames_to_paths(
                &["{ repo }/downloads/"],
                &[full_filenames],
            )),
            SourceForge => Some(apply_filenames_to_paths(
                &[
                    "{ repo }/files/binaries/{ version }",
                    "{ repo }/files/binaries/v{ version }",
                ],
                &[full_filenames, noversion_filenames],
            )),
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
