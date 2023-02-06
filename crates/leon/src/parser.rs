use std::borrow::Cow;

use crate::{Item, Literal, ParseError, Template};

impl<'s> Template<'s> {
    #[allow(clippy::should_implement_trait)] // TODO: implement FromStr
    pub fn from_str(s: &'s str) -> Result<Self, ParseError<'s>> {
        let mut tokens = Vec::new();
        let mut current = Item::Text(Literal::default());

        for (i, c) in s.chars().enumerate() {
            let check = current.clone();
            match (c, &check) {
                ('{', Item::Text(t)) => {
                    tokens.push(Item::Text(t.clone()));
                    current = Item::Key(Literal::default());
                }
                ('{', Item::Key(k)) if k.is_empty() => {
                    if let Some(Item::Text(mut t)) = tokens.pop() {
                        t.to_mut().push('{');
                        current = Item::Text(t);
                    } else {
                        return Err(ParseError {
                            src: s.into(),
                            unbalanced: Some((i, i + 1).into()),
                            empty_key: None,
                        });
                    }
                }
                ('}', Item::Key(k)) => {
                    tokens.push(Item::Key(k.clone()));
                    current = Item::Text(Literal::default());
                }
                ('}', Item::Text(k)) if k.ends_with('}') => {
                    // skip, that's the escape
                }
                (c, Item::Text(t)) => current = Item::Text(format!("{t}{c}").into()),
                (c, Item::Key(k)) => current = Item::Key(format!("{k}{c}").into()),
            }
        }

        match current {
            Item::Text(t) => tokens.push(Item::Text(t)),
            Item::Key(_) => {
                return Err(ParseError {
                    src: s.into(),
                    unbalanced: Some((s.len() - 1, s.len()).into()),
                    empty_key: None,
                })
            }
        }

        Ok(Self {
            items: tokens
                .into_iter()
                .filter_map(|tok| match tok {
                    Item::Text(t) if t.is_empty() => None,
                    Item::Key(k) if k.is_empty() => Some(Err(ParseError {
                        src: s.into(),
                        unbalanced: None,
                        empty_key: Some((s.len() - 1, s.len()).into()),
                    })),
                    Item::Key(k) => Some(Ok(Item::Key(k.trim().to_string().into()))),
                    _ => Some(Ok(tok)),
                })
                .collect::<Result<Cow<_>, _>>()?,
            default: None,
        })
    }
}

#[cfg(test)]
mod test {
    use std::borrow::Cow;

    use crate::{Item, Template};

    #[test]
    fn empty() {
        let template = Template::from_str("").unwrap();
        assert_eq!(template, Template::default());
    }

    #[test]
    fn no_keys() {
        let template = Template::from_str("hello world").unwrap();
        assert_eq!(
            template,
            Template {
                items: Cow::Borrowed(&[Item::Text("hello world".into())]),
                default: None,
            }
        );
    }

    #[test]
    fn leading_key() {
        let template = Template::from_str("{salutation} world").unwrap();
        assert_eq!(
            template,
            Template {
                items: Cow::Borrowed(&[
                    Item::Key("salutation".into()),
                    Item::Text(" world".into())
                ]),
                default: None,
            }
        );
    }

    #[test]
    fn trailing_key() {
        let template = Template::from_str("hello {name}").unwrap();
        assert_eq!(
            template,
            Template {
                items: Cow::Borrowed(&[Item::Text("hello ".into()), Item::Key("name".into())]),
                default: None,
            }
        );
    }

    #[test]
    fn middle_key() {
        let template = Template::from_str("hello {name}!").unwrap();
        assert_eq!(
            template,
            Template {
                items: Cow::Borrowed(&[
                    Item::Text("hello ".into()),
                    Item::Key("name".into()),
                    Item::Text("!".into())
                ]),
                default: None,
            }
        );
    }

    #[test]
    fn middle_text() {
        let template = Template::from_str("{salutation} good {title}").unwrap();
        assert_eq!(
            template,
            Template {
                items: Cow::Borrowed(&[
                    Item::Key("salutation".into()),
                    Item::Text(" good ".into()),
                    Item::Key("title".into()),
                ]),
                default: None,
            }
        );
    }

    #[test]
    fn multiline() {
        let template = Template::from_str(
            "
            And if thy native country was { ancient civilisation },
            What need to slight thee? Came not {hero} thence,
            Who gave to { country } her books and art of writing?
        ",
        )
        .unwrap();
        assert_eq!(
            template,
            Template {
                items: Cow::Borrowed(&[
                    Item::Text("\n            And if thy native country was ".into()),
                    Item::Key("ancient civilisation".into()),
                    Item::Text(",\n            What need to slight thee? Came not ".into()),
                    Item::Key("hero".into()),
                    Item::Text(" thence,\n            Who gave to ".into()),
                    Item::Key("country".into()),
                    Item::Text(" her books and art of writing?\n        ".into()),
                ]),
                default: None,
            }
        );
    }

    #[test]
    fn key_no_whitespace() {
        let template = Template::from_str("{word}").unwrap();
        assert_eq!(
            template,
            Template {
                items: Cow::Borrowed(&[Item::Key("word".into()),]),
                default: None,
            }
        );
    }

    #[test]
    fn key_leading_whitespace() {
        let template = Template::from_str("{ word}").unwrap();
        assert_eq!(
            template,
            Template {
                items: Cow::Borrowed(&[Item::Key("word".into()),]),
                default: None,
            }
        );
    }

    #[test]
    fn key_trailing_whitespace() {
        let template = Template::from_str("{word\n}").unwrap();
        assert_eq!(
            template,
            Template {
                items: Cow::Borrowed(&[Item::Key("word".into()),]),
                default: None,
            }
        );
    }

    #[test]
    fn key_both_whitespace() {
        let template = Template::from_str(
            "{
            \tword
        }",
        )
        .unwrap();
        assert_eq!(
            template,
            Template {
                items: Cow::Borrowed(&[Item::Key("word".into()),]),
                default: None,
            }
        );
    }

    #[test]
    fn key_inner_whitespace() {
        let template = Template::from_str("{ a word }").unwrap();
        assert_eq!(
            template,
            Template {
                items: Cow::Borrowed(&[Item::Key("a word".into()),]),
                default: None,
            }
        );
    }

    #[test]
    fn escape_left() {
        let template = Template::from_str("this {{ single left brace").unwrap();
        assert_eq!(
            template,
            Template {
                items: Cow::Borrowed(&[Item::Text("this { single left brace".into()),]),
                default: None,
            }
        );
    }

    #[test]
    fn escape_right() {
        let template = Template::from_str("this }} single right brace").unwrap();
        assert_eq!(
            template,
            Template {
                items: Cow::Borrowed(&[Item::Text("this } single right brace".into()),]),
                default: None,
            }
        );
    }

    #[test]
    fn escape_both() {
        let template = Template::from_str("these {{ two }} braces").unwrap();
        assert_eq!(
            template,
            Template {
                items: Cow::Borrowed(&[Item::Text("these { two } braces".into()),]),
                default: None,
            }
        );
    }

    #[test]
    #[cfg(fails)] // FIXME
    fn escape_doubled() {
        let template = Template::from_str("these {{{{ four }}}} braces").unwrap();
        assert_eq!(
            template,
            Template {
                items: Cow::Borrowed(&[Item::Text("these {{ four }} braces".into()),]),
                default: None,
            }
        );
    }
}
