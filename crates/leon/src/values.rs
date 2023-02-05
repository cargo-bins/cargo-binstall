use std::{
    collections::{BTreeMap, HashMap},
    hash::Hash,
};

pub trait Values<K, V> {
    fn get_value(&mut self, _key: K) -> Option<V>;
}

impl<K, V, F> Values<K, V> for F
where
    F: FnMut(K) -> Option<V> + Send + 'static,
{
    fn get_value(&mut self, key: K) -> Option<V> {
        (self)(key)
    }
}

impl<K, V> Values<K, V> for &HashMap<K, V>
where
    K: Eq + Hash,
    V: Clone,
{
    fn get_value(&mut self, key: K) -> Option<V> {
        self.get(&key).cloned()
    }
}

impl<K, V> Values<K, V> for &BTreeMap<K, V>
where
    K: Eq + Ord,
    V: Clone,
{
    fn get_value(&mut self, key: K) -> Option<V> {
        self.get(&key).cloned()
    }
}
