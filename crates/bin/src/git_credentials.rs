use std::{env, fs, path::PathBuf};

use dirs::home_dir;
use zeroize::Zeroizing;

pub fn try_from_home() -> Option<Zeroizing<Box<str>>> {
    if let Some(mut home) = home_dir() {
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
    fn test_extract_from_line() {
        GIT_CREDENTIALS_TEST_CASES.iter().for_each(|(line, res)| {
            assert_eq!(from_line(line), *res);
        })
    }
}
