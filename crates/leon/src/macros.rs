#[doc(hidden)]
#[macro_export]
macro_rules! __template_item {
    () => {};
    ({ $key:literal }) => {
        $crate::Item::Key($key)
    };
    ( $text:literal ) => {
        $crate::Item::Text($text)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __template_impl {
    ($( $token:tt ),* ; $default:expr) => {
        $crate::Template::new(
            {
                const ITEMS: &'static [$crate::Item<'static>] = &[
                    $(
                        $crate::__template_item!($token)
                    ),*
                ];
                ITEMS
            },
            $default,
        )
    };
}

/// Construct a template constant using syntax similar to the template to be
/// passed to [`Template::parse`](crate::Template::parse).
///
/// This is essentially a shorthand for:
///
/// ```
/// use leon::{Item, Template};
/// Template::new({
///     const ITEMS: &'static [Item<'static>] = &[Item::Text("Hello "), Item::Key("name")];
///     ITEMS
/// }, Some("world"));
/// ```
///
/// # Examples
///
/// ```
/// assert_eq!(
///     leon::template!("Hello ", {"name"})
///         .render(&[("name", "Магда Нахман")])
///         .unwrap(),
///     "Hello Магда Нахман",
/// );
/// ```
///
/// With a default:
///
/// ```
/// assert_eq!(
///     leon::template!("Hello ", {"name"}; "M. P. T. Acharya")
///         .render(&[("city", "Madras")])
///         .unwrap(),
///     "Hello M. P. T. Acharya",
/// );
/// ```
#[macro_export]
macro_rules! template {
    () => {
        $crate::Template::new(
            {
                const ITEMS: &'static [$crate::Item<'static>] = &[];
                ITEMS
            },
            ::core::option::Option::None,
        )
    };

    ($( $token:tt ),* $(,)?) => {
        $crate::__template_impl!($( $token ),* ; ::core::option::Option::None)
    };

    ($( $token:tt ),* $(,)? ; $default:expr) => {
        $crate::__template_impl!($( $token ),* ; ::core::option::Option::Some($default))
    };
}

#[cfg(test)]
mod tests {
    use crate::{template, Item, Template};

    #[test]
    fn test_template2() {
        assert_eq!(template!(), Template::new(&[], None),);

        // Only literals
        assert_eq!(template!("1"), Template::new(&[Item::Text("1")], None));

        assert_eq!(
            template!("1", "2"),
            Template::new(&[Item::Text("1"), Item::Text("2")], None)
        );

        assert_eq!(
            template!("1", "2", "3"),
            Template::new(&[Item::Text("1"), Item::Text("2"), Item::Text("3")], None)
        );

        // Only keys
        assert_eq!(template!({ "k1" }), Template::new(&[Item::Key("k1")], None));

        assert_eq!(
            template!({ "k1" }, { "k2" }),
            Template::new(&[Item::Key("k1"), Item::Key("k2")], None)
        );

        assert_eq!(
            template!({ "k1" }, { "k2" }, { "k3" }),
            Template::new(&[Item::Key("k1"), Item::Key("k2"), Item::Key("k3")], None)
        );

        // Mixed
        assert_eq!(
            template!("1", { "k1" }, "3"),
            Template::new(&[Item::Text("1"), Item::Key("k1"), Item::Text("3")], None)
        );

        assert_eq!(
            template!("1", "2", { "k1" }, "3", "4"),
            Template::new(
                &[
                    Item::Text("1"),
                    Item::Text("2"),
                    Item::Key("k1"),
                    Item::Text("3"),
                    Item::Text("4")
                ],
                None
            )
        );

        assert_eq!(
            template!("1", "2", { "k1" }, { "k2" }, "3", "4", { "k3" }),
            Template::new(
                &[
                    Item::Text("1"),
                    Item::Text("2"),
                    Item::Key("k1"),
                    Item::Key("k2"),
                    Item::Text("3"),
                    Item::Text("4"),
                    Item::Key("k3"),
                ],
                None
            )
        );
    }
}
