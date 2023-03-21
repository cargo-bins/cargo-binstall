use crate::{Item, ParseError, Template};

impl<'s> Template<'s> {
    pub(crate) fn parse_items(source: &'s str) -> Result<Vec<Item<'s>>, ParseError> {
        let mut items = Vec::new();

        let mut start = 0;
        let mut s = source;

        loop {
            if let Some(index) = s.find(['\\', '{', '}']) {
                if index != 0 {
                    let (first, last) = s.split_at(index);
                    items.push(Item::Text(first));

                    // Move cursor forward
                    start += index;
                    s = last;
                }
            } else {
                if !s.is_empty() {
                    items.push(Item::Text(s));
                }

                break Ok(items);
            };

            let mut chars = s.chars();
            let ch = chars.next().unwrap();

            match ch {
                '\\' => {
                    match chars.next() {
                        Some('\\' | '{' | '}') => {
                            let t = s.get(1..2).unwrap();
                            assert!(["\\", "{", "}"].contains(&t), "{}", t);
                            items.push(Item::Text(t));

                            // Move cursor forward
                            start += 2;
                            s = s.get(2..).unwrap();
                        }
                        _ => {
                            return Err(ParseError::escape(source, start, start + 1));
                        }
                    }
                }
                '{' => {
                    let Some((key, rest)) = s[1..].split_once('}') else {
                        return Err(ParseError::unbalanced(source, start, start + s.len()));
                    };
                    if let Some(index) = key.find('\\') {
                        return Err(ParseError::key_escape(source, start, start + 1 + index));
                    }

                    let k = key.trim();
                    if k.is_empty() {
                        return Err(ParseError::key_empty(source, start, start + key.len() + 1));
                    }
                    items.push(Item::Key(k));

                    // Move cursor forward
                    //       for the '{'
                    //       |              for the '}'
                    //       |               |
                    start += 1 + key.len() + 1;
                    s = rest;
                }
                '}' => {
                    return Err(ParseError::unbalanced(source, start, start));
                }
                _ => unreachable!(),
            }
        }
    }
}

#[cfg(test)]
mod test_valid {
    use crate::{template, Item::*, Template};

    #[test]
    fn empty() {
        let template = Template::parse("").unwrap();
        assert_eq!(template, Template::default());
    }

    #[test]
    fn no_keys() {
        let template = Template::parse("hello world").unwrap();
        assert_eq!(template, template!(Text("hello world")));
    }

    #[test]
    fn leading_key() {
        let template = Template::parse("{salutation} world").unwrap();
        assert_eq!(template, template!(Key("salutation"), Text(" world")));
    }

    #[test]
    fn trailing_key() {
        let template = Template::parse("hello {name}").unwrap();
        assert_eq!(template, template!(Text("hello "), Key("name")));
    }

    #[test]
    fn middle_key() {
        let template = Template::parse("hello {name}!").unwrap();
        assert_eq!(template, template!(Text("hello "), Key("name"), Text("!")));
    }

    #[test]
    fn middle_text() {
        let template = Template::parse("{salutation} good {title}").unwrap();
        assert_eq!(
            template,
            template!(Key("salutation"), Text(" good "), Key("title"))
        );
    }

    #[test]
    fn multiline() {
        let template = Template::parse(
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
                Text("\n            And if thy native country was "),
                Key("ancient civilisation"),
                Text(",\n            What need to slight thee? Came not "),
                Key("hero"),
                Text(" thence,\n            Who gave to "),
                Key("country"),
                Text(" her books and art of writing?\n        "),
            )
        );
    }

    #[test]
    fn key_no_whitespace() {
        let template = Template::parse("{word}").unwrap();
        assert_eq!(template, template!(Key("word")));
    }

    #[test]
    fn key_leading_whitespace() {
        let template = Template::parse("{ word}").unwrap();
        assert_eq!(template, template!(Key("word")));
    }

    #[test]
    fn key_trailing_whitespace() {
        let template = Template::parse("{word\n}").unwrap();
        assert_eq!(template, template!(Key("word")));
    }

    #[test]
    fn key_both_whitespace() {
        let template = Template::parse(
            "{
            \tword
        }",
        )
        .unwrap();
        assert_eq!(template, template!(Key("word")));
    }

    #[test]
    fn key_inner_whitespace() {
        let template = Template::parse("{ a word }").unwrap();
        assert_eq!(template, template!(Key("a word")));
    }

    #[test]
    fn escape_left() {
        let template = Template::parse(r"this \{ single left brace").unwrap();
        assert_eq!(
            template,
            template!(Text("this "), Text("{"), Text(" single left brace"))
        );
    }

    #[test]
    fn escape_right() {
        let template = Template::parse(r"this \} single right brace").unwrap();
        assert_eq!(
            template,
            template!(Text("this "), Text("}"), Text(" single right brace"))
        );
    }

    #[test]
    fn escape_both() {
        let template = Template::parse(r"these \{ two \} braces").unwrap();
        assert_eq!(
            template,
            template!(
                Text("these "),
                Text("{"),
                Text(" two "),
                Text("}"),
                Text(" braces")
            )
        );
    }

    #[test]
    fn escape_doubled() {
        let template = Template::parse(r"these \{\{ four \}\} braces").unwrap();
        assert_eq!(
            template,
            template!(
                Text("these "),
                Text("{"),
                Text("{"),
                Text(" four "),
                Text("}"),
                Text("}"),
                Text(" braces")
            )
        );
    }

    #[test]
    fn escape_escape() {
        let template = Template::parse(r"these \\ backslashes \\\\").unwrap();
        assert_eq!(
            template,
            template!(
                Text("these "),
                Text(r"\"),
                Text(" backslashes "),
                Text(r"\"),
                Text(r"\"),
            )
        );
    }

    #[test]
    fn escape_before_key() {
        let template = Template::parse(r"\\{ a } \{{ b } \}{ c }").unwrap();
        assert_eq!(
            template,
            template!(
                Text(r"\"),
                Key("a"),
                Text(" "),
                Text(r"{"),
                Key("b"),
                Text(" "),
                Text(r"}"),
                Key("c"),
            )
        );
    }

    #[test]
    fn escape_after_key() {
        let template = Template::parse(r"{ a }\\ { b }\{ { c }\}").unwrap();
        assert_eq!(
            template,
            template!(
                Key("a"),
                Text(r"\"),
                Text(" "),
                Key("b"),
                Text(r"{"),
                Text(" "),
                Key("c"),
                Text(r"}"),
            )
        );
    }

    #[test]
    fn multibyte_texts() {
        let template = Template::parse("幸徳 {particle} 秋水").unwrap();
        assert_eq!(
            template,
            template!(Text("幸徳 "), Key("particle"), Text(" 秋水"))
        );
    }

    #[test]
    fn multibyte_key() {
        let template = Template::parse("The { 連盟 }").unwrap();
        assert_eq!(template, template!(Text("The "), Key("連盟")));
    }

    #[test]
    fn multibyte_both() {
        let template = Template::parse("大杉 {栄}").unwrap();
        assert_eq!(template, template!(Text("大杉 "), Key("栄")));
    }

    #[test]
    fn multibyte_whitespace() {
        let template = Template::parse("岩佐　作{　太　}郎").unwrap();
        assert_eq!(template, template!(Text("岩佐　作"), Key("太"), Text("郎")));
    }

    #[test]
    fn multibyte_with_escapes() {
        let template = Template::parse(r"日本\{アナキスト\}連盟").unwrap();
        assert_eq!(
            template,
            template!(
                Text("日本"),
                Text(r"{"),
                Text("アナキスト"),
                Text(r"}"),
                Text("連盟")
            )
        );
    }

    #[test]
    fn multibyte_rtl_text() {
        let template = Template::parse("محمد صايل").unwrap();
        assert_eq!(template, template!(Text("محمد صايل")));
    }

    #[test]
    fn multibyte_rtl_key() {
        let template = Template::parse("محمد {ريشة}").unwrap();
        assert_eq!(template, template!(Text("محمد "), Key("ريشة")));
    }
}

#[cfg(test)]
mod test_error {
    use crate::{ParseError, Template};

    #[test]
    fn key_left_half() {
        let template = Template::parse("{ open").unwrap_err();
        assert_eq!(template, ParseError::unbalanced("{ open", 0, 6));
    }

    #[test]
    fn key_right_half() {
        let template = Template::parse("open }").unwrap_err();
        assert_eq!(template, ParseError::unbalanced("open }", 5, 5));
    }

    #[test]
    fn key_with_half_escape() {
        let template = Template::parse(r"this is { not \ allowed }").unwrap_err();
        assert_eq!(
            template,
            ParseError::key_escape(r"this is { not \ allowed }", 8, 14)
        );
    }

    #[test]
    fn key_with_full_escape() {
        let template = Template::parse(r"{ not \} allowed }").unwrap_err();
        assert_eq!(
            template,
            ParseError::key_escape(r"{ not \} allowed }", 0, 6)
        );
    }

    #[test]
    fn key_empty() {
        let template = Template::parse(r"void: {}").unwrap_err();
        assert_eq!(template, ParseError::key_empty(r"void: {}", 6, 7));
    }

    #[test]
    fn key_only_whitespace() {
        let template = Template::parse(r"nothing: { }").unwrap_err();
        assert_eq!(template, ParseError::key_empty(r"nothing: { }", 9, 11));
    }

    #[test]
    fn bad_escape() {
        let template = Template::parse(r"not \a thing").unwrap_err();
        assert_eq!(template, ParseError::escape(r"not \a thing", 4, 5));
    }

    #[test]
    fn end_escape() {
        let template = Template::parse(r"forget me not \").unwrap_err();
        assert_eq!(template, ParseError::escape(r"forget me not \", 14, 15));
    }
}
