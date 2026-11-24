use std::{env, fs, path::PathBuf};

use zeroize::Zeroizing;

pub fn try_from_home() -> Option<Zeroizing<Box<str>>> {
    if let Some(mut home) = env::home_dir() {
        home.push(".git-credentials");
        if let Some(cred) = from_file(home) {
            return Some(cred);
        }
    }

    if let Some(home) = env::var_os("XDG_CONFIG_HOME") {
        let mut home = PathBuf::from(home);
        home.push("git/credentials");

        if let Some(cred) = from_file(home) {
            return Some(cred);
        }
    }

    None
}

/// The host and password of each `https://` credential in the git credential
/// files of the user, whose host starts with `host_prefix`.
pub fn hosts_from_home(host_prefix: &str) -> Vec<(String, Zeroizing<Box<str>>)> {
    let mut paths = Vec::new();
    if let Some(mut home) = env::home_dir() {
        home.push(".git-credentials");
        paths.push(home);
    }
    if let Some(home) = env::var_os("XDG_CONFIG_HOME") {
        let mut home = PathBuf::from(home);
        home.push("git/credentials");
        paths.push(home);
    }

    paths
        .into_iter()
        .filter_map(|path| fs::read_to_string(path).ok().map(Zeroizing::new))
        .flat_map(|file| {
            file.lines()
                .filter_map(host_and_password)
                .filter(|(host, _)| host.starts_with(host_prefix))
                .map(|(host, password)| (host.to_owned(), Zeroizing::new(password.into())))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// `https://{username}:{password}@{host}`, maybe with a port or path.
fn host_and_password(line: &str) -> Option<(&str, &str)> {
    let (userinfo, host) = line.trim().strip_prefix("https://")?.split_once('@')?;
    let host = host.split(['/', ':']).next()?;
    let (_, password) = userinfo.split_once(':')?;
    (!host.is_empty() && !password.is_empty()).then_some((host, password))
}

fn from_file(path: PathBuf) -> Option<Zeroizing<Box<str>>> {
    Zeroizing::new(fs::read_to_string(path).ok()?)
        .lines()
        .find_map(from_line)
        .map(Box::<str>::from)
        .map(Zeroizing::new)
}

fn from_line(line: &str) -> Option<&str> {
    let cred = line
        .trim()
        .strip_prefix("https://")?
        .strip_suffix("@github.com")?;

    Some(cred.split_once(':')?.1)
}

#[cfg(test)]
mod test {
    use super::*;

    const GIT_CREDENTIALS_TEST_CASES: &[(&str, Option<&str>)] = &[
        // Success
        ("https://NobodyXu:gho_asdc@github.com", Some("gho_asdc")),
        (
            "https://NobodyXu:gho_asdc12dz@github.com",
            Some("gho_asdc12dz"),
        ),
        // Failure
        ("http://NobodyXu:gho_asdc@github.com", None),
        ("https://NobodyXu:gho_asdc@gitlab.com", None),
        ("https://NobodyXugho_asdc@github.com", None),
    ];

    #[test]
    fn test_host_and_password() {
        assert_eq!(
            host_and_password("https://oauth2:glpat-x@gitlab.example.com"),
            Some(("gitlab.example.com", "glpat-x"))
        );
        assert_eq!(
            host_and_password("https://u:p@gitlab.example.com:8443/group"),
            Some(("gitlab.example.com", "p"))
        );
        assert_eq!(host_and_password("http://u:p@gitlab.com"), None);
        assert_eq!(host_and_password("https://u@gitlab.com"), None);
    }

    #[test]
    fn test_extract_from_line() {
        GIT_CREDENTIALS_TEST_CASES.iter().for_each(|(line, res)| {
            assert_eq!(from_line(line), *res);
        })
    }
}
