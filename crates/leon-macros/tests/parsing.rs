use leon::{Item, Template};

#[test]
fn test() {
    assert_eq!(
        leon_macros::const_parse_template!(""),
        Template::new(&[], None),
    );

    assert_eq!(
        leon_macros::const_parse_template!("a"),
        Template::new(&[Item::Text("a")], None),
    );

    assert_eq!(
        leon_macros::const_parse_template!("{1}"),
        Template::new(&[Item::Key("1")], None),
    );

    assert_eq!(
        leon_macros::const_parse_template!("a{ 1 } c"),
        Template::new(&[Item::Text("a"), Item::Key("1"), Item::Text(" c")], None),
    );
}
