//! Utilities to write templates at compile time (with macros or in const contexts).
//!
//! You can write a template at compile time without any help by constructing it directly:
//!
//! ```
//! use std::borrow::Cow;
//! use leon::{Item, Literal, Template};
//! const TEMPLATE: Template = Template {
//!     items: Cow::Borrowed({
//!         const ITEMS: &'static [Item<'static>] = &[
//!             Item::Text(Literal::Borrowed("Hello")),
//!             Item::Key(Literal::Borrowed("name")),
//!         ];
//!         ITEMS
//!     }),
//!     default: None,
//! };
//! assert_eq!(TEMPLATE.render(&[("name", "world")]).unwrap(), "Helloworld");
//! ```
//!
//! But that's quite verbose. You can replace the long literals with const functions:
//!
//! ```
//! use std::borrow::Cow;
//! use leon::{Item, Template, compiletime::{default, key, template, text}};
//! const TEMPLATE: Template = template({
//!     const ITEMS: &'static [Item<'static>] = &[text("Hello "), key("name")];
//!     ITEMS
//! }, default("world"));
//!
//! assert_eq!(TEMPLATE.render(&[]).unwrap(), "Hello world");
//! ```
//!
//! That's still a bit long. Finally, you can use the `leon::template!` macro:
//!
//! ```
//! use std::borrow::Cow;
//! use leon::{Template, compiletime::{key, text}};
//! const TEMPLATE: Template = leon::template!(text("Hello "), key("name"));
//!
//! assert_eq!(TEMPLATE.render(&[("name", "Магда Нахман")]).unwrap(), "Hello Магда Нахман");
//! ```
//!
//! and with a default:
//!
//! ```
//! use std::borrow::Cow;
//! use leon::{Template, compiletime::{key, text}};
//! const TEMPLATE: Template = leon::template!(text("Hello "), key("name"); "M. P. T. Acharya");
//!
//! assert_eq!(TEMPLATE.render(&[]).unwrap(), "Hello M. P. T. Acharya");
//! ```

use std::borrow::Cow;

use crate::{Item, Literal, Template};

pub const fn template(
    items: &'static [Item<'static>],
    default: Option<Literal<'static>>,
) -> Template<'static> {
    Template {
        items: Cow::Borrowed(items),
        default,
    }
}

pub const fn default(value: &'static str) -> Option<Literal<'static>> {
    Some(Literal::Borrowed(value))
}

pub const fn text(text: &'static str) -> Item<'static> {
    Item::Text(Literal::Borrowed(text))
}

pub const fn key(key: &'static str) -> Item<'static> {
    Item::Key(Literal::Borrowed(key))
}

#[macro_export]
macro_rules! template {
    ($($item:expr),* $(,)?) => {
        $crate::compiletime::template({
            const ITEMS: &'static [$crate::Item<'static>] = &[$($item),*];
            ITEMS
        }, None)
    };
    ($($item:expr),* $(,)? ; $default:expr) => {
        $crate::compiletime::template({
            const ITEMS: &'static [$crate::Item<'static>] = &[$($item),*];
            ITEMS
        }, $crate::compiletime::default($default))
    };
}
