use std::str::FromStr;

use crate::{Item, ParseError};

enum Current {
    Text(String),
    Key(String),
}

impl FromStr for super::Template {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut tokens = Vec::new();
        let mut current = Current::Text(String::new());

        for (i, c) in s.chars().enumerate() {
            match (c, &current) {
                ('{', Current::Text(t)) => {
                    tokens.push(Item::Text(t.clone()));
                    current = Current::Key(String::new());
                }
                ('{', Current::Key(k)) if k.is_empty() => {
                    if let Some(Item::Text(mut t)) = tokens.pop() {
                        t.push('{');
                        current = Current::Text(t);
                    } else {
                        return Err(ParseError {
                            src: s.to_string(),
                            unbalanced: Some((i, i + 1).into()),
                            empty_key: None,
                        });
                    }
                }
                ('}', Current::Key(k)) => {
                    tokens.push(Item::Key(k.clone()));
                    current = Current::Text(String::new());
                }
                ('}', Current::Text(k)) if k.ends_with('}') => {
                    // skip, that's the escape
                }
                (c, Current::Text(t)) => current = Current::Text(format!("{t}{c}")),
                (c, Current::Key(k)) => current = Current::Key(format!("{k}{c}")),
            }
        }

        match current {
            Current::Text(t) => tokens.push(Item::Text(t)),
            Current::Key(_) => {
                return Err(ParseError {
                    src: s.to_string(),
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
                        src: s.to_string(),
                        unbalanced: None,
                        empty_key: Some((s.len() - 1, s.len()).into()),
                    })),
                    Item::Key(k) => Some(Ok(Item::Key(k.trim().to_string()))),
                    _ => Some(Ok(tok)),
                })
                .collect::<Result<Vec<_>, _>>()?,
            default: None,
        })
    }
}

#[cfg(test)]
mod test {
    use std::str::FromStr;

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
                items: vec![Item::Text("hello world".to_string())],
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
                items: vec![
                    Item::Key("salutation".to_string()),
                    Item::Text(" world".to_string())
                ],
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
                items: vec![
                    Item::Text("hello ".to_string()),
                    Item::Key("name".to_string())
                ],
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
                items: vec![
                    Item::Text("hello ".to_string()),
                    Item::Key("name".to_string()),
                    Item::Text("!".to_string())
                ],
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
                items: vec![
                    Item::Key("salutation".to_string()),
                    Item::Text(" good ".to_string()),
                    Item::Key("title".to_string()),
                ],
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
                items: vec![
                    Item::Text("\n            And if thy native country was ".to_string()),
                    Item::Key("ancient civilisation".to_string()),
                    Item::Text(",\n            What need to slight thee? Came not ".to_string()),
                    Item::Key("hero".to_string()),
                    Item::Text(" thence,\n            Who gave to ".to_string()),
                    Item::Key("country".to_string()),
                    Item::Text(" her books and art of writing?\n        ".to_string()),
                ],
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
                items: vec![Item::Key("word".to_string()),],
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
                items: vec![Item::Key("word".to_string()),],
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
                items: vec![Item::Key("word".to_string()),],
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
                items: vec![Item::Key("word".to_string()),],
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
                items: vec![Item::Key("a word".to_string()),],
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
                items: vec![Item::Text("this { single left brace".to_string()),],
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
                items: vec![Item::Text("this } single right brace".to_string()),],
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
                items: vec![Item::Text("these { two } braces".to_string()),],
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
                items: vec![Item::Text("these {{ four }} braces".to_string()),],
                default: None,
            }
        );
    }
}
