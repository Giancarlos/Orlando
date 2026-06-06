use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// A type map for attaching arbitrary data to a grain activation.
///
/// Used for per-grain extensions -- capabilities attached during `on_activate`
/// and accessible in handlers via `ctx.extensions()`.
///
/// Thread-safe via `Arc<RwLock<...>>` so it can be shared when `GrainContext` is cloned.
/// The Arc wrapping means clones share the same underlying map (per-activation semantics).
#[derive(Clone, Default)]
pub struct Extensions {
    inner: Arc<RwLock<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>>,
}

impl Extensions {
    /// Create an empty extensions map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert an extension value. Replaces any existing value of the same type.
    pub fn insert<T: Send + Sync + 'static>(&self, val: T) {
        let mut map = self
            .inner
            .write()
            .expect("Extensions lock poisoned on insert");
        map.insert(TypeId::of::<T>(), Arc::new(val));
    }

    /// Get an extension value by type. Returns a clone of the `Arc`.
    /// Returns `None` if no value of that type has been inserted.
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        let map = self
            .inner
            .read()
            .expect("Extensions lock poisoned on get");
        map.get(&TypeId::of::<T>())
            .and_then(|arc| arc.clone().downcast::<T>().ok())
    }

    /// Check if an extension of the given type exists.
    pub fn contains<T: Send + Sync + 'static>(&self) -> bool {
        let map = self
            .inner
            .read()
            .expect("Extensions lock poisoned on contains");
        map.contains_key(&TypeId::of::<T>())
    }

    /// Remove an extension value by type. Returns the value if it existed.
    pub fn remove<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        let mut map = self
            .inner
            .write()
            .expect("Extensions lock poisoned on remove");
        map.remove(&TypeId::of::<T>())
            .and_then(|arc| arc.downcast::<T>().ok())
    }
}

impl std::fmt::Debug for Extensions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.inner.read().map(|m| m.len()).unwrap_or(0);
        f.debug_struct("Extensions")
            .field("count", &count)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Foo(u32);
    struct Bar(String);

    #[test]
    fn insert_and_get() {
        let ext = Extensions::new();
        ext.insert(Foo(42));
        let val = ext.get::<Foo>().expect("Foo should be present");
        assert_eq!(val.0, 42);
    }

    #[test]
    fn get_missing_returns_none() {
        let ext = Extensions::new();
        assert!(ext.get::<Foo>().is_none());
    }

    #[test]
    fn insert_replaces_previous() {
        let ext = Extensions::new();
        ext.insert(Foo(1));
        ext.insert(Foo(2));
        assert_eq!(ext.get::<Foo>().unwrap().0, 2);
    }

    #[test]
    fn contains_reflects_presence() {
        let ext = Extensions::new();
        assert!(!ext.contains::<Foo>());
        ext.insert(Foo(1));
        assert!(ext.contains::<Foo>());
    }

    #[test]
    fn remove_returns_value() {
        let ext = Extensions::new();
        ext.insert(Foo(10));
        let removed = ext.remove::<Foo>().expect("should remove Foo");
        assert_eq!(removed.0, 10);
        assert!(!ext.contains::<Foo>());
    }

    #[test]
    fn remove_missing_returns_none() {
        let ext = Extensions::new();
        assert!(ext.remove::<Foo>().is_none());
    }

    #[test]
    fn multiple_types_coexist() {
        let ext = Extensions::new();
        ext.insert(Foo(1));
        ext.insert(Bar("hello".into()));
        assert_eq!(ext.get::<Foo>().unwrap().0, 1);
        assert_eq!(ext.get::<Bar>().unwrap().0, "hello");
    }

    #[test]
    fn clones_share_state() {
        let ext = Extensions::new();
        let ext2 = ext.clone();
        ext.insert(Foo(99));
        assert_eq!(ext2.get::<Foo>().unwrap().0, 99);
    }

    #[test]
    fn debug_impl() {
        let ext = Extensions::new();
        ext.insert(Foo(1));
        let dbg = format!("{:?}", ext);
        assert!(dbg.contains("count: 1"));
    }
}
