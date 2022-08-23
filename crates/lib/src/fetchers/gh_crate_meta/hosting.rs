use url::Url;

use crate::errors::BinstallError;

#[derive(Copy, Clone, Debug)]
pub enum GitHostingServices {
    GitHub,
    GitLab,
    BitBucket,
    SourceForge,
    Unknown,
}
impl GitHostingServices {
    pub fn guess_git_hosting_services(repo: &str) -> Result<Self, BinstallError> {
        use GitHostingServices::*;

        let url = Url::parse(repo)?;

        match url.domain() {
            Some(domain) if domain.starts_with("github") => Ok(GitHub),
            Some(domain) if domain.starts_with("gitlab") => Ok(GitLab),
            Some(domain) if domain == "bitbucket.org" => Ok(BitBucket),
            Some(domain) if domain == "sourceforge.net" => Ok(SourceForge),
            _ => Ok(Unknown),
        }
    }

    pub fn get_default_pkg_url_template(self) -> Option<&'static str> {
        use GitHostingServices::*;

        match self {
            GitHub => Some("{ repo }/releases/download/v{ version }/{ name }-{ target }-v{ version }.{ archive-format }"),
            GitLab => Some("{ repo }/-/releases/v{ version }/downloads/binaries/{ name }-{ target }.{ archive-format }"),
            BitBucket => Some("{ repo }/downloads/{ name }-{ target }-v{ version }.{ archive-format }"),
            SourceForge => Some("{ repo }/files/binaries/v{ version }/{ name }-{ target }.{ archive-format }/download"),
            Unknown  => None,
        }
    }
}
