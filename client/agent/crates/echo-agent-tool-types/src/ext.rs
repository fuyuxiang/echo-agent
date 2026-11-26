//! In-memory, type-keyed extension storage.
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;

// Type-erased, clone-able value stored in the Extensions map.
struct Entry {
    data: Box<dyn Any + Send + Sync>,
    clone_fn: fn(&(dyn Any + Send + Sync)) -> Box<dyn Any + Send + Sync>,
}

impl Clone for Entry {
    fn clone(&self) -> Self {
        Self {
            data: (self.clone_fn)(self.data.as_ref()),
            clone_fn: self.clone_fn,
        }
    }
}

fn clone_any<T: Clone + Send + Sync + 'static>(
    any: &(dyn Any + Send + Sync),
) -> Box<dyn Any + Send + Sync> {
    Box::new(
        any.downcast_ref::<T>()
            .expect("type mismatch in Extensions clone")
            .clone(),
    )
}

/// Clone-able, type-keyed storage for metadata.
#[derive(Default, Clone)]
pub struct Extensions {
    map: HashMap<TypeId, Entry>,
}

impl Extensions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Retrieve a reference to a stored value by type.
    pub fn get<T: Any + Send + Sync + 'static>(&self) -> Option<&T> {
        self.map
            .get(&TypeId::of::<T>())
            .and_then(|e| e.data.downcast_ref())
    }

    /// Retrieve a mutable reference to a stored value by type.
    pub fn get_mut<T: Any + Send + Sync + 'static>(&mut self) -> Option<&mut T> {
        self.map
            .get_mut(&TypeId::of::<T>())
            .and_then(|e| e.data.downcast_mut())
    }

    /// Insert a value, replacing any previous value of the same type.
    pub fn set<T: Clone + Send + Sync + 'static>(&mut self, val: T) {
        self.map.insert(
            TypeId::of::<T>(),
            Entry {
                data: Box::new(val),
                clone_fn: clone_any::<T>,
            },
        );
    }

    /// Remove and return a value by type.
    pub fn remove<T: Any + Send + Sync + 'static>(&mut self) -> Option<T> {
        self.map
            .remove(&TypeId::of::<T>())
            .and_then(|e| e.data.downcast().ok())
            .map(|b| *b)
    }

    /// Check if a value of the given type is stored.
    pub fn contains<T: Any + Send + Sync + 'static>(&self) -> bool {
        self.map.contains_key(&TypeId::of::<T>())
    }

    /// Number of stored entries.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Returns true if no entries are stored.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

// Extensions are runtime-only metadata, not part of serialized identity.
impl PartialEq for Extensions {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for Extensions {}

impl fmt::Debug for Extensions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Extensions")
            .field("len", &self.map.len())
            .finish()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    struct LocalToolContext {
        user_id: String,
        conversation_id: String,
    }

    #[test]
    fn set_and_get() {
        let mut ext = Extensions::new();
        ext.set(LocalToolContext {
            user_id: "123".into(),
            conversation_id: "456".into(),
        });

        let hints = ext.get::<LocalToolContext>().unwrap();
        assert_eq!(hints.user_id, "123");
        assert_eq!(hints.conversation_id, "456");
    }

    #[test]
    fn get_mut() {
        let mut ext = Extensions::new();
        ext.set(LocalToolContext {
            user_id: "123".into(),
            conversation_id: "456".into(),
        });

        ext.get_mut::<LocalToolContext>().unwrap().conversation_id = "789".into();
        assert_eq!(
            ext.get::<LocalToolContext>().unwrap().conversation_id,
            "789"
        );
    }

    #[test]
    fn missing_returns_none() {
        let ext = Extensions::new();
        assert!(ext.get::<LocalToolContext>().is_none());
        assert!(!ext.contains::<LocalToolContext>());
    }

    #[test]
    fn clone_preserves_values() {
        let mut ext = Extensions::new();
        ext.set(LocalToolContext {
            user_id: "123".into(),
            conversation_id: "456".into(),
        });

        let cloned = ext.clone();
        assert_eq!(cloned.get::<LocalToolContext>().unwrap().user_id, "123");
        assert_eq!(
            cloned.get::<LocalToolContext>().unwrap().conversation_id,
            "456"
        );
    }
}
