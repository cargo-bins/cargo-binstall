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
        end: usize,
    },
    Escape {
        start: usize,
        end: usize,
        ch: Option<char>,
    },
}

impl Token {
    fn start_text(pos: usize, ch: char) -> Self {
        Self::Text {
            start: pos,
            end: pos + ch.len_utf8(),
        }
    }

    fn start_text_single(pos: usize) -> Self {
        Self::Text {
            start: pos,
            end: pos,
        }
    }

    fn start_brace_pair(pos: usize, ch: char) -> Self {
        Self::BracePair {
            start: pos,
            key_seen: false,
            end: pos + ch.len_utf8(),
        }
    }

    fn start_escape(pos: usize, ch: char) -> Self {
        Self::Escape {
            start: pos,
            end: pos + ch.len_utf8(),
            ch: None,
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
                end,
            } => !key_seen || *start >= source_len || *end >= source_len || *start > *end,
            Self::Escape { start, end, .. } => {
                *start >= source_len || *end >= source_len || *start > *end
            }
        }
    }

    fn start(&self) -> usize {
        match self {
            Self::Text { start, .. }
            | Self::BracePair { start, .. }
            | Self::Escape { start, .. } => *start,
        }
    }

    fn end(&self) -> usize {
        match self {
            Self::Text { end, .. } | Self::BracePair { end, .. } | Self::Escape { end, .. } => *end,
        }
    }

    fn set_end(&mut self, pos: usize) {
        match self {
            Self::Text { end, .. } | Self::BracePair { end, .. } | Self::Escape { end, .. } => {
                *end = pos
            }
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
    // TODO: figure out if it's parseable without cow (escapes can just point to the source)
    #[allow(clippy::should_implement_trait)] // TODO: implement FromStr
    pub fn from_str(s: &'s str) -> Result<Self, ParseError<'s>> {
        let source_len = s.len();
        let mut tokens = Vec::new();

        let mut current = Token::start_text(0, '\0');

        for (pos, chara) in s.char_indices() {
            match (&mut current, chara) {
                (tok @ (Token::Text { .. } | Token::Escape { ch: Some(_), .. }), ch @ '{') => {
                    if matches!(tok, Token::Text { .. }) && tok.start() == pos {
                        eprintln!("bracepair new | pos={pos:2}  replace tok={tok:?}");
                        *tok = Token::start_brace_pair(pos, ch);
                    } else {
                        eprintln!("bracepair new | pos={pos:2}     push tok={tok:?}");
                        tokens.push(replace(tok, Token::start_brace_pair(pos, ch)));
                    }
                }
                (txt @ Token::Text { .. }, ch @ '\\') => {
                    if txt.is_empty(source_len) || txt.start() == pos {
                        *txt = Token::start_escape(pos, ch);
                    } else {
                        tokens.push(replace(txt, Token::start_escape(pos, ch)));
                    }
                }
                (bp @ Token::BracePair { .. }, '}') => {
                    if let Token::BracePair { end, .. } = bp {
                        *end = pos;
                    } else {
                        unreachable!("bracepair isn't bracepair");
                    }

                    tokens.push(replace(bp, Token::start_text_single(pos + 1)));
                }
                (Token::BracePair { start, .. }, '\\') => {
                    return Err(ParseError::key_escape(s, *start, pos));
                }
                (
                    Token::BracePair {
                        key_seen,
                        start,
                        end,
                    },
                    ws,
                ) if ws.is_whitespace() => {
                    eprintln!("bracepair ws  > pos={pos:2}   key seen={key_seen} start={start:2} end={end:2}");
                    if *key_seen {
                        *end = pos;
                    } else {
                        // We're in a brace pair, but we're not in the key yet.
                    }
                    eprintln!("bracepair ws  < pos={pos:2}   key seen={key_seen} start={start:2} end={end:2}");
                }
                (
                    Token::BracePair {
                        key_seen,
                        start,
                        end,
                    },
                    _,
                ) => {
                    eprintln!("bracepair any > pos={pos:2}   key seen={key_seen} start={start:2} end={end:2}");
                    *key_seen = true;
                    *end = pos + 1;
                    eprintln!("bracepair any < pos={pos:2}   key seen={key_seen} start={start:2} end={end:2}");
                }
                (Token::Text { start, end, .. }, ch) => {
                    eprintln!(
                        "text any      > pos={pos:2}   start={start:2} end={end:2}  ch={ch:?}"
                    );
                    *end = pos;
                    eprintln!(
                        "text any      < pos={pos:2}   start={start:2} end={end:2}  ch={ch:?}"
                    );
                }
                (esc @ Token::Escape { .. }, es @ ('\\' | '{' | '}')) => {
                    if let Token::Escape { start, end, ch, .. } = esc {
                        if ch.is_none() {
                            eprintln!(
                                "escape valid  > pos={pos:2}   start={start:2} end={end:2}  ch={ch:?}"
                            );
                            *end = pos;
                            *ch = Some(es);
                            eprintln!(
                                "escape valid  < pos={pos:2}   start={start:2} end={end:2}  ch={ch:?}"
                            );
                        } else if es == '\\' {
                            // A new escape right after a completed escape.
                            eprintln!(
                                "escape new    | pos={pos:2}   start={start:2} end={end:2}  ch={ch:?}"
                            );
                            tokens.push(replace(esc, Token::start_escape(pos, es)));
                        } else if es == '{' {
                            // A new brace pair right after a completed escape, should be handled prior to this.
                            unreachable!("escape followed by brace pair, unhandled");
                        } else {
                            // } right after a completed escape, probably unreachable but just in case:
                            return Err(ParseError::key_escape(s, *start, pos));
                        }
                    } else {
                        unreachable!("escape is not an escape");
                    }
                }
                (
                    Token::Escape {
                        start,
                        end,
                        ch: None,
                    },
                    ch,
                ) => {
                    eprintln!(
                        "escape error  | pos={pos:2}   start={start:2} end={end:2}  ch={ch:?}"
                    );
                    return Err(ParseError::escape(s, *start, pos));
                }
                (
                    Token::Escape {
                        start,
                        end,
                        ch: Some(_),
                    },
                    ch,
                ) => {
                    eprintln!(
                        "escape after  | pos={pos:2}   start={start:2} end={end:2}  ch={ch:?}"
                    );
                    tokens.push(replace(&mut current, Token::start_text_single(pos)));
                }
            }
        }

        dbg!(s, source_len);
        dbg!(&tokens, &current);

        if !current.is_empty(source_len) {
            if current.end() < source_len - 1 {
                current.set_end(source_len - 1);
            }
            dbg!(current.debug(s));

            tokens.push(current);
        }
        dbg!(
            tokens.iter().map(|t| t.debug(s)).collect::<Vec<_>>(),
            current.debug(s)
        );

        let mut items = Vec::new();
        for token in tokens {
            match token {
                Token::Text { start, end } => {
                    items.push(Item::Text(Literal::Borrowed(&s[start..=end])));
                }
                Token::BracePair {
                    start,
                    end,
                    key_seen: false,
                } => {
                    return Err(ParseError::key_empty(s, start, end));
                }
                Token::BracePair {
                    start,
                    end,
                    key_seen: true,
                } => {
                    let key = s[start..=end]
                        .trim_matches(|c: char| c.is_whitespace() || c == '{' || c == '}');
                    if key.is_empty() {
                        return Err(ParseError::key_empty(s, start, end));
                    } else {
                        items.push(Item::Key(Literal::Borrowed(key)));
                    }
                }
                Token::Escape { ch: Some(ch), .. } => {
                    items.push(Item::Text(Literal::Owned(ch.to_string())));
                }
                Token::Escape {
                    ch: None,
                    start,
                    end,
                } => {
                    return Err(ParseError::escape(s, start, end));
                }
            }
        }

        Ok(Template {
            items: Cow::Owned(items),
            default: None,
        })
    }
}

impl<'s> ParseError<'s> {
    fn unbalanced(source: &'s str, start: usize, end: usize) -> Self {
        Self {
            src: Literal::Borrowed(source),
            unbalanced: Some((start, end).into()),
            escape: None,
            key_empty: None,
            key_escape: None,
        }
    }

    fn escape(source: &'s str, start: usize, end: usize) -> Self {
        Self {
            src: Literal::Borrowed(source),
            unbalanced: None,
            escape: Some((start, end).into()),
            key_empty: None,
            key_escape: None,
        }
    }

    fn key_empty(source: &'s str, start: usize, end: usize) -> Self {
        Self {
            src: Literal::Borrowed(source),
            unbalanced: None,
            escape: None,
            key_empty: Some((start, end).into()),
            key_escape: None,
        }
    }

    fn key_escape(source: &'s str, start: usize, end: usize) -> Self {
        Self {
            src: Literal::Borrowed(source),
            unbalanced: None,
            escape: None,
            key_empty: None,
            key_escape: Some((start, end).into()),
        }
    }
}

#[cfg(test)]
mod test_valid {
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
        let template = Template::from_str(r"this \{ single left brace").unwrap();
        assert_eq!(
            template,
            template!(text("this "), text("{"), text(" single left brace"))
        );
    }

    #[test]
    fn escape_right() {
        let template = Template::from_str(r"this \} single right brace").unwrap();
        assert_eq!(
            template,
            template!(text("this "), text("}"), text(" single right brace"))
        );
    }

    #[test]
    fn escape_both() {
        let template = Template::from_str(r"these \{ two \} braces").unwrap();
        assert_eq!(
            template,
            template!(
                text("these "),
                text("{"),
                text(" two "),
                text("}"),
                text(" braces")
            )
        );
    }

    #[test]
    fn escape_doubled() {
        let template = Template::from_str(r"these \{\{ four \}\} braces").unwrap();
        assert_eq!(
            template,
            template!(
                text("these "),
                text("{"),
                text("{"),
                text(" four "),
                text("}"),
                text("}"),
                text(" braces")
            )
        );
    }

    #[test]
    fn escape_escape() {
        let template = Template::from_str(r"these \\ backslashes \\\\").unwrap();
        assert_eq!(
            template,
            template!(
                text("these "),
                text(r"\"),
                text(" backslashes "),
                text(r"\"),
                text(r"\"),
            )
        );
    }

    #[test]
    fn escape_before_key() {
        let template = Template::from_str(r"\\{ a } \{{ b } \}{ c }").unwrap();
        assert_eq!(
            template,
            template!(
                text(r"\"),
                key("a"),
                text(" "),
                text(r"{"),
                key("b"),
                text(" "),
                text(r"}"),
                key("c"),
            )
        );
    }

    #[test]
    fn escape_after_key() {
        let template = Template::from_str(r"{ a }\\ { b }\{ { c }\}").unwrap();
        assert_eq!(
            template,
            template!(
                key("a"),
                text(r"\"),
                text(" "),
                key("b"),
                text(r"{"),
                text(" "),
                key("c"),
                text(r"}"),
            )
        );
    }

    #[test]
    fn multibyte_texts() {
        let template = Template::from_str("幸徳 {particle} 秋水").unwrap();
        assert_eq!(
            template,
            template!(text("幸徳 "), key("particle"), text(" 秋水"))
        );
    }

    #[test]
    fn multibyte_key() {
        let template = Template::from_str("The { 連盟 }").unwrap();
        assert_eq!(template, template!(text("The "), key("連盟")));
    }

    #[test]
    fn multibyte_both() {
        let template = Template::from_str("大杉 {栄}").unwrap();
        assert_eq!(template, template!(text("大杉"), key("栄")));
    }

    // #[test]
    // fn multibyte_whitespace() {
    //     let template = Template::from_str("岩佐　作{　太　}郎").unwrap();
    //     assert_eq!(template, template!(text("岩佐　作"), key("太"), text("郎")));
    // }

    #[test]
    fn multibyte_rtl_text() {
        let template = Template::from_str("محمد صايل").unwrap();
        assert_eq!(template, template!(text("محمد صايل")));
    }

    // #[test]
    // fn multibyte_rtl_key() {
    //     let template = Template::from_str("محمد {ريشة}").unwrap();
    //     assert_eq!(template, template!(text("محمد "), key("ريشة")));
    // }
}
