use std::{
    borrow::{Borrow, Cow},
    collections::{BTreeMap, HashMap},
    hash::{BuildHasher, Hash},
};

pub trait Values {
    fn get_value(&self, key: &str) -> Option<Cow<'_, str>>;
}

impl<T> Values for &T
where
    T: Values,
{
    fn get_value(&self, key: &str) -> Option<Cow<'_, str>> {
        T::get_value(self, key)
    }
}

impl<K, V> Values for [(K, V)]
where
    K: AsRef<str>,
    V: AsRef<str>,
{
    fn get_value(&self, key: &str) -> Option<Cow<'_, str>> {
        self.iter().find_map(|(k, v)| {
            if k.as_ref() == key {
                Some(Cow::Borrowed(v.as_ref()))
            } else {
                None
            }
        })
    }
}

impl<K, V> Values for &[(K, V)]
where
    K: AsRef<str>,
    V: AsRef<str>,
{
    fn get_value(&self, key: &str) -> Option<Cow<'_, str>> {
        (*self).get_value(key)
    }
}

impl<K, V, const N: usize> Values for [(K, V); N]
where
    K: AsRef<str>,
    V: AsRef<str>,
{
    fn get_value(&self, key: &str) -> Option<Cow<'_, str>> {
        self.as_slice().get_value(key)
    }
}

impl<K, V> Values for Vec<(K, V)>
where
    K: AsRef<str>,
    V: AsRef<str>,
{
    fn get_value(&self, key: &str) -> Option<Cow<'_, str>> {
        self.as_slice().get_value(key)
    }
}

impl<K, V, S> Values for HashMap<K, V, S>
where
    K: Borrow<str> + Eq + Hash,
    V: AsRef<str>,
    S: BuildHasher,
{
    fn get_value(&self, key: &str) -> Option<Cow<'_, str>> {
        self.get(key).map(|v| Cow::Borrowed(v.as_ref()))
    }
}

impl<K, V> Values for BTreeMap<K, V>
where
    K: Borrow<str> + Ord,
    V: AsRef<str>,
{
    fn get_value(&self, key: &str) -> Option<Cow<'_, str>> {
        self.get(key).map(|v| Cow::Borrowed(v.as_ref()))
    }
}

/// Workaround to allow using functions as [`Values`].
///
/// As this isn't constructible you'll want to use [`vals()`] instead.
pub struct ValuesFn<F> {
    inner: F,
}

impl<'s, F> Values for &'s ValuesFn<F>
where
    F: Fn(&str) -> Option<Cow<'s, str>> + 's,
{
    fn get_value(&self, key: &str) -> Option<Cow<'_, str>> {
        (self.inner)(key)
    }
}

impl<'f, F> From<F> for ValuesFn<F>
where
    F: Fn(&str) -> Option<Cow<'f, str>> + 'f,
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
/// use_values(&vals(|_| Some("hello".into())));
/// ```
pub const fn vals<'f, F>(func: F) -> ValuesFn<F>
where
    F: Fn(&str) -> Option<Cow<'f, str>> + 'f,
{
    ValuesFn { inner: func }
}
