//! Tokens for git hosting websites other than GitHub, see
//! [`binstalk::helpers::release_assets::ForgeTokens`].

use std::env;

use binstalk::helpers::release_assets::{ForgeTokens, GitLabToken};
use zeroize::Zeroizing;

use crate::git_credentials;

/// The tokens from `--gitlab-token`/`GITLAB_TOKEN` (`token`), GitLab CI and
/// the git credential files of the user.
///
/// Each is only used for its own host.
pub fn discover(token: Option<Zeroizing<Box<str>>>) -> ForgeTokens {
    let mut tokens = ForgeTokens::default();

    if let Some(token) = token.filter(|token| !token.is_empty()) {
        // As `glab` does.
        let host = env::var("GITLAB_HOST").ok();
        let host = host.as_deref().map(host_of).unwrap_or("gitlab.com");
        tokens.add_gitlab(host, GitLabToken::access_token(token));
    }

    // Set by GitLab CI for every job.
    if let (Some(token), Ok(host)) = (
        env::var_os("CI_JOB_TOKEN").and_then(|token| token.into_string().ok()),
        env::var("CI_SERVER_HOST"),
    ) && !token.is_empty()
    {
        let token = Zeroizing::new(token.into_boxed_str());
        tokens.add_gitlab(&host, GitLabToken::job_token(token));
    }

    // Only hosts that are guessed to be GitLab are asked with a token.
    for (host, password) in git_credentials::hosts_from_home("gitlab") {
        tokens.add_gitlab(&host, GitLabToken::access_token(password));
    }

    tokens
}

/// The host of `GITLAB_HOST`, which may be a URL.
fn host_of(host: &str) -> &str {
    let host = host
        .strip_prefix("https://")
        .or_else(|| host.strip_prefix("http://"))
        .unwrap_or(host);
    host.split(['/', ':']).next().unwrap_or(host)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_host_of() {
        assert_eq!(host_of("gitlab.example.com"), "gitlab.example.com");
        assert_eq!(host_of("https://gitlab.example.com/"), "gitlab.example.com");
        assert_eq!(
            host_of("http://gitlab.example.com:8080"),
            "gitlab.example.com"
        );
    }
}
