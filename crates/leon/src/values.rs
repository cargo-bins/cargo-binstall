use std::{
    borrow::{Borrow, Cow},
    collections::{BTreeMap, HashMap},
    hash::{BuildHasher, Hash},
};

pub trait Values {
    fn get_value<'s, 'k: 's>(&'s self, key: &'k str) -> Option<Cow<'s, str>>;
}

impl<T> Values for &T
where
    T: Values,
{
    fn get_value<'s, 'k: 's>(&'s self, key: &'k str) -> Option<Cow<'s, str>> {
        T::get_value(self, key)
    }
}

impl Values for [(&str, &str)] {
    fn get_value<'s, 'k: 's>(&'s self, key: &'k str) -> Option<Cow<'s, str>> {
        self.iter().find_map(|(k, v)| {
            if k == &key {
                Some(Cow::Borrowed(*v))
            } else {
                None
            }
        })
    }
}

impl<const N: usize> Values for [(&str, &str); N] {
    fn get_value<'s, 'k: 's>(&'s self, key: &'k str) -> Option<Cow<'s, str>> {
        self.iter().find_map(|(k, v)| {
            if k == &key {
                Some(Cow::Borrowed(*v))
            } else {
                None
            }
        })
    }
}

impl Values for Vec<(&str, &str)> {
    fn get_value<'s, 'k: 's>(&'s self, key: &'k str) -> Option<Cow<'s, str>> {
        self.iter().find_map(|(k, v)| {
            if k == &key {
                Some(Cow::Borrowed(*v))
            } else {
                None
            }
        })
    }
}

impl<K, V, S> Values for HashMap<K, V, S>
where
    K: Borrow<str> + Eq + Hash,
    V: AsRef<str>,
    S: BuildHasher,
{
    fn get_value<'s, 'k: 's>(&'s self, key: &'k str) -> Option<Cow<'s, str>> {
        self.get(key).map(|v| Cow::Borrowed(v.as_ref()))
    }
}

impl<K, V> Values for BTreeMap<K, V>
where
    K: Borrow<str> + Ord,
    V: AsRef<str>,
{
    fn get_value<'s, 'k: 's>(&'s self, key: &'k str) -> Option<Cow<'s, str>> {
        self.get(key).map(|v| Cow::Borrowed(v.as_ref()))
    }
}

impl Values for [(String, &str)] {
    fn get_value<'s, 'k: 's>(&'s self, key: &'k str) -> Option<Cow<'s, str>> {
        self.iter().find_map(|(k, v)| {
            if k == key {
                Some(Cow::Borrowed(*v))
            } else {
                None
            }
        })
    }
}

impl<const N: usize> Values for [(String, &str); N] {
    fn get_value<'s, 'k: 's>(&'s self, key: &'k str) -> Option<Cow<'s, str>> {
        self.iter().find_map(|(k, v)| {
            if k == key {
                Some(Cow::Borrowed(*v))
            } else {
                None
            }
        })
    }
}

impl Values for Vec<(String, &str)> {
    fn get_value<'s, 'k: 's>(&'s self, key: &'k str) -> Option<Cow<'s, str>> {
        self.iter().find_map(|(k, v)| {
            if k == key {
                Some(Cow::Borrowed(*v))
            } else {
                None
            }
        })
    }
}

impl Values for [(String, String)] {
    fn get_value<'s, 'k: 's>(&'s self, key: &'k str) -> Option<Cow<'s, str>> {
        self.iter().find_map(|(k, v)| {
            if k == key {
                Some(Cow::Owned(v.clone()))
            } else {
                None
            }
        })
    }
}

impl<const N: usize> Values for [(String, String); N] {
    fn get_value<'s, 'k: 's>(&'s self, key: &'k str) -> Option<Cow<'s, str>> {
        self.iter().find_map(|(k, v)| {
            if k == key {
                Some(Cow::Borrowed(v.as_str()))
            } else {
                None
            }
        })
    }
}

impl Values for Vec<(String, String)> {
    fn get_value<'s, 'k: 's>(&'s self, key: &'k str) -> Option<Cow<'s, str>> {
        self.iter().find_map(|(k, v)| {
            if k == key {
                Some(Cow::Owned(v.clone()))
            } else {
                None
            }
        })
    }
}

/// Workaround to allow using functions as [`Values`].
///
/// As this isn't constructible you'll want to use [`vals()`] instead.
pub struct ValuesFn<F>
where
    F: for<'s> Fn(&'s str) -> Option<Cow<'s, str>> + Send + 'static,
{
    inner: F,
}

impl<F> Values for ValuesFn<F>
where
    F: for<'s> Fn(&'s str) -> Option<Cow<'s, str>> + Send + 'static,
{
    fn get_value<'s, 'k: 's>(&'s self, key: &'k str) -> Option<Cow<'s, str>> {
        (self.inner)(key)
    }
}

impl<F> From<F> for ValuesFn<F>
where
    F: for<'s> Fn(&'s str) -> Option<Cow<'s, str>> + Send + 'static,
{
    fn from(inner: F) -> Self {
        Self { inner }
    }
}

/// Workaround to allow using functions as [`Values`].
///
/// Wraps your function so it implements [`Values`].
///
/// # Example
///
/// ```
/// use leon::{Values, vals};
///
/// fn use_values(_values: impl Values) {}
///
/// use_values(vals(|_| Some("hello".into())));
/// ```
pub const fn vals<F>(func: F) -> ValuesFn<F>
where
    F: for<'s> Fn(&'s str) -> Option<Cow<'s, str>> + Send + 'static,
{
    ValuesFn { inner: func }
}
