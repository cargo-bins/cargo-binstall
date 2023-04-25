use std::{
    borrow::{Borrow, Cow},
    collections::{BTreeMap, HashMap},
    hash::{BuildHasher, Hash},
    ops::Deref,
    rc::Rc,
    sync::Arc,
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

impl<T> Values for Arc<T>
where
    T: Values,
{
    fn get_value(&self, key: &str) -> Option<Cow<'_, str>> {
        T::get_value(self.deref(), key)
    }
}

impl<T> Values for Rc<T>
where
    T: Values,
{
    fn get_value(&self, key: &str) -> Option<Cow<'_, str>> {
        T::get_value(self.deref(), key)
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
#[derive(Copy, Clone, Debug)]
pub struct ValuesFn<F> {
    inner: F,
}

impl<F> Values for ValuesFn<F>
where
    F: Fn(&str) -> Option<Cow<'static, str>>,
{
    fn get_value(&self, key: &str) -> Option<Cow<'_, str>> {
        (self.inner)(key)
    }
}

/// See doc of [`vals`]
impl<F> From<F> for ValuesFn<F>
where
    F: Fn(&str) -> Option<Cow<'static, str>>,
{
    fn from(inner: F) -> Self {
        Self { inner }
    }
}

/// Wraps your function so it implements [`Values`],
/// though it only works if your function returns `Cow<'static, str>`.
///
/// Since regular function pointers cannot return anything other than
/// `Cow<'static, str>` and closure in Rust currently does not support
/// returning borrows of captured data, supporting anything other than
/// `Cow<'static, str>` for functions is pointless and would only cause
/// more confusion and compile-time errors.
///
/// To return `&str` owned by the values itself, please create a newtype
/// and implement [`Values`] on it manually instead of using this function.
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
pub const fn vals<F>(func: F) -> ValuesFn<F>
where
    F: Fn(&str) -> Option<Cow<'static, str>>,
{
    ValuesFn { inner: func }
}
