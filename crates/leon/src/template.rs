use std::{io::Write, ops::Add};

use crate::Values;

use super::LeonError;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Template {
    pub items: Vec<Item>,
    pub default: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Text(String),
    Key(String),
}

impl Template {
    pub fn render_into<'a>(
        &'a self,
        writer: &mut dyn Write,
        mut values: impl Values<&'a str, &'a str>,
    ) -> Result<(), LeonError> {
        for token in &self.items {
            match token {
                Item::Text(text) => writer.write_all(text.as_bytes())?,
                Item::Key(key) => {
                    if let Some(value) = values.get_value(key) {
                        writer.write_all(value.as_bytes())?;
                    } else if let Some(default) = &self.default {
                        writer.write_all(default.as_bytes())?;
                    } else {
                        return Err(LeonError::MissingKey(key.clone()));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn render<'a>(&'a self, values: impl Values<&'a str, &'a str>) -> Result<String, LeonError> {
        let mut buf = Vec::with_capacity(
            self.items
                .iter()
                .map(|item| match item {
                    Item::Key(_) => 0,
                    Item::Text(t) => t.len(),
                })
                .sum(),
        );
        self.render_into(&mut buf, values)?;

        // UNWRAP: We know that the buffer is valid UTF-8 because we only write strings.
        Ok(String::from_utf8(buf).unwrap())
    }

    pub fn has_key(&self, key: &str) -> bool {
        self.items.iter().any(|token| match token {
            Item::Key(k) => k == key,
            _ => false,
        })
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.items.iter().filter_map(|token| match token {
            Item::Key(k) => Some(k.as_str()),
            _ => None,
        })
    }
}

impl Add for Template {
    type Output = Self;

    fn add(mut self, rhs: Self) -> Self::Output {
        self.items.extend(rhs.items);
        if let Some(default) = rhs.default {
            self.default = Some(default);
        }
        self
    }
}

#[cfg(test)]
mod test {
    use crate::{Template, Item};

    #[test]
    fn concat_templates() {
        let t1 = Template {
            items: vec![Item::Text("Hello".to_string()), Item::Key("name".to_string())],
            default: None,
        };
        let t2 = Template {
            items: vec![Item::Text("have a".to_string()), Item::Key("adjective".to_string()), Item::Text("day!".to_string())],
            default: None,
        };
        assert_eq!(t1 + t2, Template {
            items: vec![
                Item::Text("Hello".to_string()),
                Item::Key("name".to_string()),
                Item::Text("have a".to_string()),
                Item::Key("adjective".to_string()),
                Item::Text("day!".to_string()),
            ],
            default: None,
        });
    }
}
