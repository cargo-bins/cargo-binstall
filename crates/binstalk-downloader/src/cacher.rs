use std::{
    any::{Any, TypeId},
    collections::{BTreeMap, HashMap},
    fmt::Debug,
    future::Future,
    sync::{Arc, RwLock},
};

use tokio::sync::OnceCell;
use url::Url;

type ErasedCachedEntry = Arc<dyn Any + Send + Sync>;

#[derive(Debug, Default)]
struct TypesMap {
    /// Store the first element inline to avoid heap allocation.
    first: Option<(TypeId, ErasedCachedEntry)>,
    map: BTreeMap<TypeId, ErasedCachedEntry>,
}

impl TypesMap {
    fn get(&self, type_id: TypeId) -> Option<ErasedCachedEntry> {
        match &self.first {
            Some((tid, entry)) if *tid == type_id => Some(entry.clone()),
            _ => self.map.get(&type_id).cloned(),
        }
    }

    fn insert(&mut self, type_id: TypeId, f: fn() -> ErasedCachedEntry) -> ErasedCachedEntry {
        if self.first.is_none() {
            debug_assert!(self.map.is_empty());

            let entry = f();
            self.first = Some((type_id, entry.clone()));
            entry
        } else {
            self.map.entry(type_id).or_insert_with(f).clone()
        }
    }
}

#[derive(Debug, Default)]
struct Map(HashMap<Url, TypesMap>);

impl Map {
    fn get(&self, url: &Url, type_id: TypeId) -> Option<ErasedCachedEntry> {
        self.0.get(url).and_then(|types_map| types_map.get(type_id))
    }

    fn insert(
        &mut self,
        url: Url,
        type_id: TypeId,
        f: fn() -> ErasedCachedEntry,
    ) -> ErasedCachedEntry {
        self.0.entry(url).or_default().insert(type_id, f)
    }
}

/// Provides a multi-value hashmap to store results of (processed) response of
/// http requests for each url.
///
/// The cached value can be arbitrary type and there can be multiple cached
/// values.
#[derive(Clone, Debug, Default)]
pub struct HTTPCacher(Arc<RwLock<Map>>);

impl HTTPCacher {
    fn try_get_entry(&self, url: &Url, type_id: TypeId) -> Option<ErasedCachedEntry> {
        self.0.read().unwrap().get(url, type_id)
    }

    fn get_entry_inner(
        &self,
        url: &Url,
        type_id: TypeId,
        f: fn() -> ErasedCachedEntry,
    ) -> ErasedCachedEntry {
        if let Some(entry) = self.try_get_entry(url, type_id) {
            entry
        } else {
            // Clone the url first to reduce critical section
            let url = url.clone();
            self.0.write().unwrap().insert(url, type_id, f)
        }
    }

    pub fn get_entry<T: Send + Sync + 'static>(&self, url: &Url) -> HTTPCachedEntry<T> {
        HTTPCachedEntry(
            self.get_entry_inner(url, TypeId::of::<T>(), || Arc::<OnceCell<T>>::default())
                .downcast()
                .expect("BUG: The type of value mismatches the type id in the key"),
        )
    }
}

#[derive(Clone, Debug)]
pub struct HTTPCachedEntry<T>(Arc<OnceCell<T>>);

impl<T> HTTPCachedEntry<T> {
    pub async fn get_or_try_init<E, F, Fut>(&self, f: F) -> Result<&T, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, E>>,
    {
        self.0.get_or_try_init(f).await
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[derive(Debug, Eq, PartialEq, Copy, Clone)]
    struct T1(u32);

    #[derive(Debug, Eq, PartialEq, Copy, Clone)]
    struct T2(u32);

    #[tokio::test]
    async fn test() {
        let cacher = HTTPCacher::default();
        let url1 = Url::parse("https://example.com").unwrap();
        let url2 = Url::parse("https://example2.com").unwrap();

        // Get but don't initialize
        cacher.get_entry::<T1>(&url1);

        // Get but initialization failure.
        cacher
            .get_entry::<T1>(&url1)
            .get_or_try_init(|| async { Err(()) })
            .await
            .unwrap_err();

        // Initialize it
        assert_eq!(
            cacher
                .get_entry::<T1>(&url1)
                .get_or_try_init(|| async { Ok::<_, ()>(T1(232)) })
                .await
                .copied()
                .unwrap(),
            T1(232)
        );

        // Make sure it stay initialized
        assert_eq!(
            cacher
                .get_entry::<T1>(&url1)
                .get_or_try_init(|| async { Ok::<_, ()>(T1(2322)) })
                .await
                .copied()
                .unwrap(),
            T1(232)
        );

        // Try a different type and make sure it is a different entry
        assert_eq!(
            cacher
                .get_entry::<T2>(&url1)
                .get_or_try_init(|| async { Ok::<_, ()>(T2(9012)) })
                .await
                .copied()
                .unwrap(),
            T2(9012)
        );

        // Try a different url
        assert_eq!(
            cacher
                .get_entry::<T2>(&url2)
                .get_or_try_init(|| async { Ok::<_, ()>(T2(239012)) })
                .await
                .copied()
                .unwrap(),
            T2(239012)
        );
    }
}
