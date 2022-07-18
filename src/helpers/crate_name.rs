use std::convert::Infallible;
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone)]
pub struct CrateName {
    pub name: String,
    pub version: Option<String>,
}

impl fmt::Display for CrateName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)?;

        if let Some(version) = &self.version {
            write!(f, "@{version}")?;
        }

        Ok(())
    }
}

impl FromStr for CrateName {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(if let Some((name, version)) = s.split_once('@') {
            CrateName {
                name: name.to_string(),
                version: Some(version.to_string()),
            }
        } else {
            CrateName {
                name: s.to_string(),
                version: None,
            }
        })
    }
}
