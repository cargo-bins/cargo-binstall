use std::{
    fmt,
    ops::{Deref, DerefMut},
};

use zeroize::Zeroizing;

#[repr(transparent)]
#[derive(Clone, Default)]
pub struct Redacted<T>(T);

impl<T> Redacted<T> {
    pub const fn new(value: T) -> Self {
        Self(value)
    }
}

impl Redacted<Zeroizing<String>> {
    pub fn from_string(value: String) -> Self {
        Self::new(Zeroizing::new(value))
    }

    pub fn from_boxed_str(value: Box<str>) -> Self {
        Self::from_string(value.into())
    }
}

impl<T> Deref for Redacted<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for Redacted<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

pub type SecretString = Redacted<Zeroizing<String>>;
