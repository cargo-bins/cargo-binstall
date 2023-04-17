//! Dead-simple string templating.
//!
//! Leon parses a template string into a list of tokens, and then substitutes
//! provided values in. Unlike other templating engines, it is extremely simple:
//! it supports no logic, only replaces. It is even simpler than `format!()`,
//! albeit with a similar syntax.
//!
//! # Syntax
//!
//! ```plain
//! it is better to rule { group }
//! one can live {adverb} without power
//! ```
//!
//! A replacement is denoted by `{` and `}`. The contents of the braces, trimmed
//! of any whitespace, are the key. Any text outside of braces is left as-is.
//!
//! To escape a brace, use `\{` or `\}`. To escape a backslash, use `\\`. Keys
//! cannot contain escapes.
//!
//! ```plain
//! \{ leon \}
//! ```
//!
//! The above examples, given the values `group = "no one"` and
//! `adverb = "honourably"`, would render to:
//!
//! ```plain
//! it is better to rule no one
//! one can live honourably without power
//! { leon }
//! ```
//!
//! # Usage
//!
//! A template is first parsed to a token list:
//!
//! ```
//! use leon::Template;
//!
//! let template = Template::parse("hello {name}").unwrap();
//! ```
//!
//! The template can be inspected, for example to check if a key is present:
//!
//! ```
//! # use leon::Template;
//! #
//! # let template = Template::parse("hello {name}").unwrap();
//! assert!(template.has_key("name"));
//! ```
//!
//! The template can be rendered to a string:
//!
//! ```
//! # use leon::Template;
//! use leon::vals;
//! #
//! # let template = Template::parse("hello {name}").unwrap();
//! assert_eq!(
//!     template.render(
//!         &&vals(|_key| Some("marcus".into()))
//!     ).unwrap().as_str(),
//!     "hello marcus",
//! );
//! ```
//!
//! …or to a writer:
//!
//! ```
//! use std::io::Write;
//! # use leon::Template;
//! use leon::vals;
//! #
//! # let template = Template::parse("hello {name}").unwrap();
//! let mut buf: Vec<u8> = Vec::new();
//! template.render_into(
//!     &mut buf,
//!     &&vals(|key| if key == "name" {
//!         Some("julius".into())
//!     } else {
//!         None
//!     })
//! ).unwrap();
//! assert_eq!(buf.as_slice(), b"hello julius");
//! ```
//!
//! …with a map:
//!
//! ```
//! use std::collections::HashMap;
//! # use leon::Template;
//! # let template = Template::parse("hello {name}").unwrap();
//! let mut values = HashMap::new();
//! values.insert("name", "brutus");
//! assert_eq!(template.render(&values).unwrap().as_str(), "hello brutus");
//! ```
//!
//! …or with your own type, if you implement the [`Values`] trait:
//!
//! ```
//! # use leon::Template;
//! use std::borrow::Cow;
//! use leon::Values;
//!
//! struct MyMap {
//!   name: &'static str,
//! }
//! impl Values for MyMap {
//!    fn get_value(&self, key: &str) -> Option<Cow<'_, str>> {
//!       if key == "name" {
//!         Some(self.name.into())
//!      } else {
//!        None
//!     }
//!    }
//! }
//! #
//! # let template = Template::parse("hello {name}").unwrap();
//! let values = MyMap { name: "pontifex" };
//! assert_eq!(template.render(&values).unwrap().as_str(), "hello pontifex");
//! ```
//!
//! # Compile-time parsing
//!
//! You can either use [`leon-macros`](https://docs.rs/leon-macros)'s
//! [`template!`](https://docs.rs/leon-macros/latest/leon_macros/macro.template.html),
//! a proc-macro, with the exact same syntax as the normal parser, or this
//! crate's [`template!`] rules-macro, which requires a slightly different
//! syntax but doesn't bring in additional dependencies. In either case,
//! the leon library is required as a runtime dependency.
//!
//! # Errors
//!
//! Leon will return a [`ParseError`] if the template fails to
//! parse. This can happen if there are unbalanced braces, or if a key is empty.
//!
//! Leon will return a [`RenderError::MissingKey`] if a key is missing from keyed
//! values passed to [`Template::render()`], unless a default value is provided
//! with [`Template.default`].
//!
//! It will also pass through I/O errors when using [`Template::render_into()`].

#[doc(inline)]
pub use error::*;

#[doc(inline)]
pub use template::*;

#[doc(inline)]
pub use values::*;

mod error;
mod macros;
mod parser;
mod template;
mod values;
