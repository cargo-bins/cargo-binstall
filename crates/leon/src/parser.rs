use std::{borrow::Cow, mem::replace};

use crate::{Item, Literal, ParseError, Template};

#[derive(Debug, Clone, Copy)]
enum Token {
    Text {
        start: usize,
        end: usize,
    },
    BracePair {
        start: usize,
        key_seen: bool,
        key_start: usize,
        key_end: usize,
        end: usize,
    },
    // Escape { start: usize, end: usize },
}

impl Token {
    fn start_text(pos: usize) -> Self {
        Self::Text {
            start: pos,
            end: pos + 1,
        }
    }

    fn start_brace_pair(pos: usize) -> Self {
        Self::BracePair {
            start: pos,
            key_seen: false,
            key_start: pos + 1,
            key_end: pos + 1,
            end: pos + 1,
        }
    }

    fn is_empty(&self, source_len: usize) -> bool {
        match self {
            Self::Text { start, end } => {
                *start >= source_len || *end >= source_len || *start > *end
            }
            Self::BracePair {
                start,
                key_seen,
                key_start,
                key_end,
                end,
            } => {
                !key_seen
                    || *start >= source_len
                    || *end >= source_len
                    || *key_start >= source_len
                    || *key_end >= source_len
                    || *start > *end
                    || *key_start > *key_end
            }
        }
    }

    fn start(&self) -> usize {
        match self {
            Self::Text { start, .. } => *start,
            Self::BracePair { start, .. } => *start,
        }
    }

    fn end(&self) -> usize {
        match self {
            Self::Text { end, .. } => *end,
            Self::BracePair { end, .. } => *end,
        }
    }

    fn debug<'a>(&'a self, source: &'a str) -> (&str, &Self) {
        (
            if self.is_empty(source.len()) {
                ""
            } else {
                &source[(self.start())..=(self.end())]
            },
            self,
        )
    }
}

impl<'s> Template<'s> {
    #[allow(clippy::should_implement_trait)] // TODO: implement FromStr
    pub fn from_str(s: &'s str) -> Result<Self, ParseError<'s>> {
        let mut tokens = Vec::new();

        let mut current = Token::start_text(0);

        for (pos, chara) in s.char_indices() {
            match (&mut current, chara) {
                (txt @ Token::Text { .. }, '{') => {
                    if txt.start() == pos {
                        *txt = Token::start_brace_pair(pos);
                    } else {
                        tokens.push(replace(txt, Token::start_brace_pair(pos)));
                    }
                }
                (bp @ Token::BracePair { .. }, '}') => {
                    tokens.push(replace(bp, Token::start_text(pos + 1)));
                }
                (
                    Token::BracePair {
                        key_seen,
                        key_start,
                        key_end,
                        end,
                        ..
                    },
                    ws,
                ) if ws.is_whitespace() => {
                    eprintln!("bracepair ws  > pos={pos}   key seen={key_seen} start={key_start} end={key_end}");
                    if *key_seen {
                        *key_end = pos - 1;
                        *end = pos;
                    } else {
                        // We're in a brace pair, but we're not in the key yet.
                        *key_start = pos + 1;
                    }
                    eprintln!("bracepair ws  < pos={pos}   key seen={key_seen} start={key_start} end={key_end}");
                }
                (
                    Token::BracePair {
                        key_seen,
                        key_start,
                        key_end,
                        end,
                        ..
                    },
                    _,
                ) => {
                    eprintln!("bracepair any > pos={pos}   key seen={key_seen} start={key_start} end={key_end}");
                    *key_seen = true;
                    *key_end = pos;
                    *end = pos + 1;
                    eprintln!("bracepair any < pos={pos}   key seen={key_seen} start={key_start} end={key_end}");
                }
                (Token::Text { end, .. }, _) => {
                    *end = pos;
                }
            }
        }

        let source_len = s.len();
        dbg!(s, source_len);
        dbg!(tokens.iter().map(|t| t.debug(s)).collect::<Vec<_>>());
        dbg!(current.debug(s));

        if !current.is_empty(source_len) {
            tokens.push(current);
        }

        let mut items = Vec::new();
        for token in tokens {
            match token {
                Token::Text { start, end } => {
                    items.push(Item::Text(Literal::Borrowed(&s[start..=end])));
                }
                Token::BracePair {
                    key_start, key_end, ..
                } => {
                    items.push(Item::Key(Literal::Borrowed(s[key_start..=key_end].trim())));
                }
            }
        }

        Ok(Template {
            items: Cow::Owned(items),
            default: None,
        })
    }
}

#[cfg(test)]
mod test {
    use std::borrow::Cow;

    use crate::{helpers::*, template, Template};

    #[test]
    fn empty() {
        let template = Template::from_str("").unwrap();
        assert_eq!(template, Template::default());
    }

    #[test]
    fn no_keys() {
        let template = Template::from_str("hello world").unwrap();
        assert_eq!(template, template!(text("hello world")));
    }

    #[test]
    fn leading_key() {
        let template = Template::from_str("{salutation} world").unwrap();
        assert_eq!(template, template!(key("salutation"), text(" world")));
    }

    #[test]
    fn trailing_key() {
        let template = Template::from_str("hello {name}").unwrap();
        assert_eq!(template, template!(text("hello "), key("name")));
    }

    #[test]
    fn middle_key() {
        let template = Template::from_str("hello {name}!").unwrap();
        assert_eq!(template, template!(text("hello "), key("name"), text("!")));
    }

    #[test]
    fn middle_text() {
        let template = Template::from_str("{salutation} good {title}").unwrap();
        assert_eq!(
            template,
            template!(key("salutation"), text(" good "), key("title"))
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
            template!(
                text("\n            And if thy native country was "),
                key("ancient civilisation"),
                text(",\n            What need to slight thee? Came not "),
                key("hero"),
                text(" thence,\n            Who gave to "),
                key("country"),
                text(" her books and art of writing?\n        "),
            )
        );
    }

    #[test]
    fn key_no_whitespace() {
        let template = Template::from_str("{word}").unwrap();
        assert_eq!(template, template!(key("word")));
    }

    #[test]
    fn key_leading_whitespace() {
        let template = Template::from_str("{ word}").unwrap();
        assert_eq!(template, template!(key("word")));
    }

    #[test]
    fn key_trailing_whitespace() {
        let template = Template::from_str("{word\n}").unwrap();
        assert_eq!(template, template!(key("word")));
    }

    #[test]
    fn key_both_whitespace() {
        let template = Template::from_str(
            "{
            \tword
        }",
        )
        .unwrap();
        assert_eq!(template, template!(key("word")));
    }

    #[test]
    fn key_inner_whitespace() {
        let template = Template::from_str("{ a word }").unwrap();
        assert_eq!(template, template!(key("a word")));
    }

    #[test]
    fn escape_left() {
        let template = Template::from_str("this {{ single left brace").unwrap();
        assert_eq!(template, template!(text("this { single left brace")));
    }

    #[test]
    fn escape_right() {
        let template = Template::from_str("this }} single right brace").unwrap();
        assert_eq!(template, template!(text("this } single right brace")));
    }

    #[test]
    fn escape_both() {
        let template = Template::from_str("these {{ two }} braces").unwrap();
        assert_eq!(template, template!(text("these { two } braces")));
    }

    #[test]
    fn escape_doubled() {
        let template = Template::from_str("these {{{{ four }}}} braces").unwrap();
        assert_eq!(template, template!(text("these {{ four }} braces")));
    }

    // TODO: multibyte
}
