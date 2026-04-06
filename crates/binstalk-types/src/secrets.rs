use std::{fmt, ops::Deref};

use zeroize::Zeroizing;

#[repr(transparent)]
#[derive(Clone, Default)]
pub struct Redacted<T>(T);

impl<T> Redacted<T> {
    pub const fn new(value: T) -> Self {
        Self(value)
    }
}

impl Redacted<Zeroizing<Box<str>>> {
    pub fn from_boxed_str(value: Box<str>) -> Self {
        Self::new(Zeroizing::new(value))
    }
}

impl<T> Deref for Redacted<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

pub type SecretString = Redacted<Zeroizing<Box<str>>>;
