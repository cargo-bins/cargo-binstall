use std::{
    collections::{BTreeMap, HashMap},
    hash::Hash,
    marker::PhantomData,
};

pub trait Values<K, V> {
    fn get_value(&self, key: K) -> Option<V>;
}

impl<K, V, T> Values<K, V> for &T
where
    T: Values<K, V>,
{
    fn get_value(&self, key: K) -> Option<V> {
        T::get_value(self, key)
    }
}

impl<K, V> Values<K, V> for [(K, V)]
where
    K: Eq,
    V: Clone,
{
    fn get_value(&self, key: K) -> Option<V> {
        self.iter()
            .find_map(|(k, v)| if k == &key { Some(v.clone()) } else { None })
    }
}

impl<K, V, const N: usize> Values<K, V> for [(K, V); N]
where
    K: Eq,
    V: Clone,
{
    fn get_value(&self, key: K) -> Option<V> {
        self.iter()
            .find_map(|(k, v)| if k == &key { Some(v.clone()) } else { None })
    }
}

impl<K, V> Values<K, V> for Vec<(K, V)>
where
    K: Eq,
    V: Clone,
{
    fn get_value(&self, key: K) -> Option<V> {
        self.iter()
            .find_map(|(k, v)| if k == &key { Some(v.clone()) } else { None })
    }
}

impl<K, V> Values<K, V> for HashMap<K, V>
where
    K: Eq + Hash,
    V: Clone,
{
    fn get_value(&self, key: K) -> Option<V> {
        self.get(&key).cloned()
    }
}

impl<K, V> Values<K, V> for BTreeMap<K, V>
where
    K: Eq + Ord,
    V: Clone,
{
    fn get_value(&self, key: K) -> Option<V> {
        self.get(&key).cloned()
    }
}

/// Workaround to allow using functions as [`Values`].
///
/// As this isn't constructible you'll want to use [`vals()`] instead.
pub struct ValuesFn<K, V, F>
where
    F: Fn(K) -> Option<V> + Send + 'static,
{
    inner: F,
    _k: PhantomData<K>,
    _v: PhantomData<V>,
}

impl<K, V, F> Values<K, V> for ValuesFn<K, V, F>
where
    F: Fn(K) -> Option<V> + Send + 'static,
{
    fn get_value(&self, key: K) -> Option<V> {
        (self.inner)(key)
    }
}

impl<K, V, F> From<F> for ValuesFn<K, V, F>
where
    F: Fn(K) -> Option<V> + Send + 'static,
{
    fn from(inner: F) -> Self {
        Self {
            inner,
            _k: PhantomData,
            _v: PhantomData,
        }
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
/// fn use_values(_values: impl Values<(), ()>) {}
///
/// use_values(vals(|_| Some(())));
/// ```
pub const fn vals<K, V, F>(func: F) -> ValuesFn<K, V, F>
where
    F: Fn(K) -> Option<V> + Send + 'static,
{
    ValuesFn {
        inner: func,
        _k: PhantomData,
        _v: PhantomData,
    }
}
