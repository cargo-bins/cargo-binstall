/// Construct a template constant without needing to make an items constant.
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
/// use leon::Item::*;
/// assert_eq!(
///     leon::template!(Text("Hello "), Key("name"))
///         .render(&[("name", "Магда Нахман")])
///         .unwrap(),
///     "Hello Магда Нахман",
/// );
/// ```
///
/// With a default:
///
/// ```
/// use leon::Item::*;
/// assert_eq!(
///     leon::template!(Text("Hello "), Key("name"); "M. P. T. Acharya")
///         .render(&[("city", "Madras")])
///         .unwrap(),
///     "Hello M. P. T. Acharya",
/// );
/// ```
#[macro_export]
macro_rules! template {
    ($($item:expr),* $(,)?) => {
        $crate::Template::new({
            const ITEMS: &'static [$crate::Item<'static>] = &[$($item),*];
            ITEMS
        }, ::core::option::Option::None)
    };
    ($($item:expr),* $(,)? ; $default:expr) => {
        $crate::Template::new({
            const ITEMS: &'static [$crate::Item<'static>] = &[$($item),*];
            ITEMS
        }, ::core::option::Option::Some($default))
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __template2_item {
    () => {};
    ({ $key:literal }) => {
        $crate::Item::Key($key)
    };
    ( $text:literal ) => {
        $crate::Item::Text($text)
    };
}

/// Construct a template constant using syntax similar to the template to be
/// passed to [`Template::parse`].
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
///     leon::template2!("Hello ", {"name"})
///         .render(&[("name", "Магда Нахман")])
///         .unwrap(),
///     "Hello Магда Нахман",
/// );
/// ```
#[macro_export]
macro_rules! template2 {
    () => {
        $crate::Template::new(
            {
                const ITEMS: &'static [$crate::Item<'static>] = &[];
                ITEMS
            },
            ::core::option::Option::None,
        )
    };

    ($( $token:tt),*) => {
        $crate::Template::new(
            {
                const ITEMS: &'static [$crate::Item<'static>] = &[
                    $(
                        $crate::__template2_item!($token)
                    ),*
                ];
                ITEMS
            },
            ::core::option::Option::None,
        )
    };
}

#[cfg(test)]
mod tests {
    use crate::{template2, Item, Template};

    #[test]
    fn test_template2() {
        assert_eq!(template2!(), Template::new(&[], None),);

        // Only literals
        assert_eq!(template2!("1"), Template::new(&[Item::Text("1")], None));

        assert_eq!(
            template2!("1", "2"),
            Template::new(&[Item::Text("1"), Item::Text("2")], None)
        );

        assert_eq!(
            template2!("1", "2", "3"),
            Template::new(&[Item::Text("1"), Item::Text("2"), Item::Text("3")], None)
        );

        // Only keys
        assert_eq!(
            template2!({ "k1" }),
            Template::new(&[Item::Key("k1")], None)
        );

        assert_eq!(
            template2!({ "k1" }, { "k2" }),
            Template::new(&[Item::Key("k1"), Item::Key("k2")], None)
        );

        assert_eq!(
            template2!({ "k1" }, { "k2" }, { "k3" }),
            Template::new(&[Item::Key("k1"), Item::Key("k2"), Item::Key("k3")], None)
        );

        // Mixed
        assert_eq!(
            template2!("1", { "k1" }, "3"),
            Template::new(&[Item::Text("1"), Item::Key("k1"), Item::Text("3")], None)
        );

        assert_eq!(
            template2!("1", "2", { "k1" }, "3", "4"),
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
            template2!("1", "2", { "k1" }, { "k2" }, "3", "4", { "k3" }),
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
