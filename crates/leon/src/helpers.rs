//! Utilities to reduce boilerplate when writing templates literals.
//!
//! You can write a template literal without any help by constructing it directly:
//!
//! ```
//! use std::borrow::Cow;
//! use leon::{Item, Literal, Template};
//! const TEMPLATE: Template = Template {
//!     items: Cow::Borrowed({
//!         const ITEMS: &'static [Item<'static>] = &[
//!             Item::Text("Hello"),
//!             Item::Key("name"),
//!         ];
//!         ITEMS
//!     }),
//!     default: None,
//! };
//! assert_eq!(TEMPLATE.render(&[("name", "world")]).unwrap(), "Helloworld");
//! ```
//!
//! That's a bit verbose. You can replace the long literals with const functions:
//!
//! ```
//! use std::borrow::Cow;
//! use leon::{Item, Template, helpers::{default, key, template, text}};
//! const TEMPLATE: Template = template({
//!     const ITEMS: &'static [Item<'static>] = &[text("Hello "), key("name")];
//!     ITEMS
//! }, default("world"));
//!
//! assert_eq!(TEMPLATE.render(&[]).unwrap(), "Hello world");
//! ```
//!
//! Finally, you can use the `leon::template!` macro:
//!
//! ```
//! use std::borrow::Cow;
//! use leon::{Template, helpers::{key, text}};
//! const TEMPLATE: Template = leon::template!(text("Hello "), key("name"));
//!
//! assert_eq!(TEMPLATE.render(&[("name", "Магда Нахман")]).unwrap(), "Hello Магда Нахман");
//! ```
//!
//! and with a default:
//!
//! ```
//! use std::borrow::Cow;
//! use leon::{Template, helpers::{key, text}};
//! const TEMPLATE: Template = leon::template!(text("Hello "), key("name"); "M. P. T. Acharya");
//!
//! assert_eq!(TEMPLATE.render(&[]).unwrap(), "Hello M. P. T. Acharya");
//! ```

use std::borrow::Cow;

use crate::{Item, Literal, Template};

/// Construct a template with the given items and default.
pub const fn template<'s>(items: &'s [Item<'s>], default: Option<Literal<'s>>) -> Template<'s> {
    Template {
        items: Cow::Borrowed(items),
        default,
    }
}

/// Construct a literal suitable for use as a default.
pub const fn default<'s>(value: &'s str) -> Option<Literal<'s>> {
    Some(Literal::Borrowed(value))
}

/// Construct a template text literal.
pub const fn text<'s>(text: &'s str) -> Item<'s> {
    Item::Text(text)
}

/// Construct a template key literal.
pub const fn key<'s>(key: &'s str) -> Item<'s> {
    Item::Key(key)
}

/// Construct a template constant without needing to make an items constant.
///
/// This is essentially a shorthand for:
///
/// ```
/// # use std::borrow::Cow;
/// # use leon::{Item, Template, helpers::{default, key, template, text}};
/// # const TEMPLATE: Template =
/// template({
///     const ITEMS: &'static [Item<'static>] = &[text("Hello "), key("name")];
///     ITEMS
/// }, default("world"));
/// ```
///
/// # Examples
///
/// ```
/// # use std::borrow::Cow;
/// # use leon::{Template, helpers::{key, text}};
/// # const TEMPLATE: Template =
/// leon::template!(text("Hello "), key("name"));
/// # const WITH_DEFAULT: Template =
/// leon::template!(text("Hello "), key("name"); "with default");
/// ```
///
#[macro_export]
macro_rules! template {
    ($($item:expr),* $(,)?) => {
        $crate::helpers::template({
            const ITEMS: &'static [$crate::Item<'static>] = &[$($item),*];
            ITEMS
        }, None)
    };
    ($($item:expr),* $(,)? ; $default:expr) => {
        $crate::helpers::template({
            const ITEMS: &'static [$crate::Item<'static>] = &[$($item),*];
            ITEMS
        }, $crate::helpers::default($default))
    };
}
