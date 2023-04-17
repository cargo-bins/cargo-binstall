use leon::{Item, Template};

#[test]
fn test() {
    assert_eq!(leon_macros::template!(""), Template::new(&[], None),);

    assert_eq!(
        leon_macros::template!("a"),
        Template::new(&[Item::Text("a")], None),
    );

    assert_eq!(
        leon_macros::template!("{1}"),
        Template::new(&[Item::Key("1")], None),
    );

    assert_eq!(
        leon_macros::template!("a{ 1 } c"),
        Template::new(&[Item::Text("a"), Item::Key("1"), Item::Text(" c")], None),
    );
}
