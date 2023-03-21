use std::{borrow::Cow, fmt::Display, io::Write, ops::Add};

use crate::{ParseError, RenderError, Values};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Template<'s> {
    pub items: Cow<'s, [Item<'s>]>,
    pub default: Option<Cow<'s, str>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item<'s> {
    Text(&'s str),
    Key(&'s str),
}

impl<'s> Template<'s> {
    /// Construct a template with the given items and default.
    ///
    /// You can write a template literal without any help by constructing it directly:
    ///
    /// ```
    /// use std::borrow::Cow;
    /// use leon::{Item, Template};
    /// const TEMPLATE: Template = Template {
    ///     items: Cow::Borrowed({
    ///         const ITEMS: &'static [Item<'static>] = &[
    ///             Item::Text("Hello"),
    ///             Item::Key("name"),
    ///         ];
    ///         ITEMS
    ///     }),
    ///     default: None,
    /// };
    /// assert_eq!(TEMPLATE.render(&[("name", "world")]).unwrap(), "Helloworld");
    /// ```
    ///
    /// As that's a bit verbose, using this function and the enum shorthands can be helpful:
    ///
    /// ```
    /// use leon::{Item, Item::*, Template};
    /// const TEMPLATE: Template = Template::new({
    ///     const ITEMS: &'static [Item<'static>] = &[Text("Hello "), Key("name")];
    ///     ITEMS
    /// }, Some("world"));
    ///
    /// assert_eq!(TEMPLATE.render(&[("unrelated", "value")]).unwrap(), "Hello world");
    /// ```
    ///
    /// For an even more ergonomic syntax, see the [`leon::template!`] macro.
    pub const fn new(items: &'s [Item<'s>], default: Option<&'s str>) -> Template<'s> {
        Template {
            items: Cow::Borrowed(items),
            default: match default {
                Some(default) => Some(Cow::Borrowed(default)),
                None => None,
            },
        }
    }

    /// Parse a template from a string.
    ///
    /// # Syntax
    ///
    /// ```plain
    /// it is better to rule { group }
    /// one can live {adverb} without power
    /// ```
    ///
    /// A replacement is denoted by `{` and `}`. The contents of the braces, trimmed
    /// of any whitespace, are the key. Any text outside of braces is left as-is.
    ///
    /// To escape a brace, use `\{` or `\}`. To escape a backslash, use `\\`. Keys
    /// cannot contain escapes.
    ///
    /// ```plain
    /// \{ leon \}
    /// ```
    ///
    /// The above examples, given the values `group = "no one"` and
    /// `adverb = "honourably"`, would render to:
    ///
    /// ```plain
    /// it is better to rule no one
    /// one can live honourably without power
    /// { leon }
    /// ```
    ///
    /// # Example
    ///
    /// ```
    /// use leon::Template;
    /// let template = Template::parse("hello {name}").unwrap();
    /// ```
    ///
    pub fn parse(s: &'s str) -> Result<Self, ParseError> {
        Self::parse_items(s).map(|items| Template {
            items: Cow::Owned(items),
            default: None,
        })
    }

    pub fn render_into(
        &self,
        writer: &mut dyn Write,
        values: &dyn Values,
    ) -> Result<(), RenderError> {
        for token in self.items.as_ref() {
            match token {
                Item::Text(text) => writer.write_all(text.as_bytes())?,
                Item::Key(key) => {
                    if let Some(value) = values.get_value(key) {
                        writer.write_all(value.as_bytes())?;
                    } else if let Some(default) = &self.default {
                        writer.write_all(default.as_bytes())?;
                    } else {
                        return Err(RenderError::MissingKey(key.to_string()));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn render(&self, values: &dyn Values) -> Result<String, RenderError> {
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
        self.has_keys(&[key])
    }

    pub fn has_keys(&self, keys: &[&str]) -> bool {
        self.items.iter().any(|token| match token {
            Item::Key(k) => keys.contains(k),
            _ => false,
        })
    }

    pub fn keys(&self) -> impl Iterator<Item = &&str> {
        self.items.iter().filter_map(|token| match token {
            Item::Key(k) => Some(k),
            _ => None,
        })
    }

    /// Sets the default value for this template.
    pub fn set_default(&mut self, default: &dyn Display) {
        self.default = Some(Cow::Owned(default.to_string()));
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
    use crate::Item::{Key, Text};

    #[test]
    fn concat_templates() {
        let t1 = crate::template!(Text("Hello"), Key("name"));
        let t2 = crate::template!(Text("have a"), Key("adjective"), Text("day"));
        assert_eq!(
            t1 + t2,
            crate::template!(
                Text("Hello"),
                Key("name"),
                Text("have a"),
                Key("adjective"),
                Text("day")
            ),
        );
    }
}
