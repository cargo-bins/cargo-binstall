use std::{fmt, str::FromStr};

use compact_str::CompactString;
use itertools::Itertools;
use semver::{Error, VersionReq};

use super::version_ext::VersionReqExt;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CrateName {
    pub name: CompactString,
    pub version_req: Option<VersionReq>,
}

impl fmt::Display for CrateName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)?;

        if let Some(version) = &self.version_req {
            write!(f, "@{version}")?;
        }

        Ok(())
    }
}

impl FromStr for CrateName {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(if let Some((name, version)) = s.split_once('@') {
            CrateName {
                name: name.into(),
                version_req: Some(VersionReq::parse_from_cli(version)?),
            }
        } else {
            CrateName {
                name: s.into(),
                version_req: None,
            }
        })
    }
}

impl CrateName {
    pub fn dedup(mut crate_names: Vec<Self>) -> impl Iterator<Item = Self> {
        crate_names.sort_by(|x, y| x.name.cmp(&y.name));
        crate_names.into_iter().coalesce(|previous, current| {
            if previous.name == current.name {
                Ok(current)
            } else {
                Err((previous, current))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! assert_dedup {
        ([ $( ( $input_name:expr, $input_version:expr ) ),*  ], [ $( ( $output_name:expr, $output_version:expr ) ),*  ]) => {
            let input_crate_names = vec![$( CrateName {
                name: $input_name.into(),
                version_req: Some($input_version.parse().unwrap())
            }, )*];

            let mut output_crate_names: Vec<CrateName> = vec![$( CrateName {
                name: $output_name.into(), version_req: Some($output_version.parse().unwrap())
            }, )*];
            output_crate_names.sort_by(|x, y| x.name.cmp(&y.name));

            let crate_names: Vec<_> = CrateName::dedup(input_crate_names).collect();
            assert_eq!(crate_names, output_crate_names);
        };
    }

    #[test]
    fn test_dedup() {
        // Base case 0: Empty input
        assert_dedup!([], []);

        // Base case 1: With only one input
        assert_dedup!([("a", "1")], [("a", "1")]);

        // Base Case 2: Only has duplicate names
        assert_dedup!([("a", "1"), ("a", "2")], [("a", "2")]);

        // Complex Case 0: Having two crates
        assert_dedup!(
            [("a", "10"), ("b", "3"), ("a", "0"), ("b", "0"), ("a", "1")],
            [("a", "1"), ("b", "0")]
        );

        // Complex Case 1: Having three crates
        assert_dedup!(
            [
                ("d", "1.1"),
                ("a", "10"),
                ("b", "3"),
                ("d", "230"),
                ("a", "0"),
                ("b", "0"),
                ("a", "1"),
                ("d", "23")
            ],
            [("a", "1"), ("b", "0"), ("d", "23")]
        );
    }
}
