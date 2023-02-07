use std::{borrow::Cow, io::Write, ops::Add};

use crate::{LeonError, Values};

pub type Literal<'s> = Cow<'s, str>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Template<'s> {
    pub items: Cow<'s, [Item<'s>]>,
    pub default: Option<Literal<'s>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item<'s> {
    Text(Literal<'s>),
    Key(Literal<'s>),
}

impl<'s> Template<'s> {
    pub fn render_into<'a>(
        &'a self,
        writer: &mut dyn Write,
        values: impl Values<&'a str, &'a str>,
    ) -> Result<(), LeonError> {
        for token in self.items.as_ref() {
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

    pub fn render<'a>(
        &'a self,
        values: impl Values<&'a str, &'a str>,
    ) -> Result<String, LeonError> {
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

    pub fn keys(&self) -> impl Iterator<Item = &Literal> {
        self.items.iter().filter_map(|token| match token {
            Item::Key(k) => Some(k),
            _ => None,
        })
    }
}

impl<'s> Add for Template<'s> {
    type Output = Self;

    fn add(mut self, rhs: Self) -> Self::Output {
        self.items
            .to_mut()
            .extend(rhs.items.as_ref().iter().cloned());
        if let Some(default) = rhs.default {
            self.default = Some(default);
        }
        self
    }
}

#[cfg(test)]
mod test {
    use crate::helpers::{key, text};

    #[test]
    fn concat_templates() {
        let t1 = crate::template!(text("Hello"), key("name"));
        let t2 = crate::template!(text("have a"), key("adjective"), text("day"));
        assert_eq!(
            t1 + t2,
            crate::template!(
                text("Hello"),
                key("name"),
                text("have a"),
                key("adjective"),
                text("day")
            ),
        );
    }
}
