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
