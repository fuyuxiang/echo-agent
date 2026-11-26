extern crate lru;
use lru::LruCache;
use std::borrow::Borrow;
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};

#[test]
fn cache_remains_usable_after_a_key_destructor_panics() {
    static PANICKED: AtomicBool = AtomicBool::new(false);
    #[derive(Eq, PartialEq)]
    struct Key(i32);
    impl Hash for Key {
        fn hash<H: Hasher>(&self, state: &mut H) {
            self.0.hash(state);
        }
    }
    impl Borrow<i32> for Key {
        fn borrow(&self) -> &i32 {
            &self.0
        }
    }
    impl Drop for Key {
        fn drop(&mut self) {
            if self.0 == 1 && !PANICKED.swap(true, Ordering::SeqCst) {
                panic!("key destructor");
            }
        }
    }
    let mut cache = LruCache::new(NonZeroUsize::new(2).unwrap());
    cache.put(Key(1), 1);
    cache.put(Key(2), 2);
    assert!(catch_unwind(AssertUnwindSafe(|| cache.pop(&1))).is_err());
    assert_eq!(cache.get(&2), Some(&2));
    cache.put(Key(3), 3);
    cache.put(Key(4), 4);
    assert_eq!(cache.len(), 2);
    assert_eq!(cache.get(&4), Some(&4));
    assert_eq!(cache.get(&3), Some(&3));
}

#[test]
fn mutable_iteration_preserves_earlier_key_and_value_references() {
    let mut cache = LruCache::new(NonZeroUsize::new(3).unwrap());
    cache.put(1, 10);
    cache.put(2, 20);
    cache.put(3, 30);
    let mut iter = cache.iter_mut();
    let (first, first_value) = iter.next().unwrap();
    let (last, last_value) = iter.next_back().unwrap();
    *first_value += 1;
    *last_value += 2;
    assert_eq!((*first, *first_value), (3, 31));
    assert_eq!((*last, *last_value), (1, 12));
    assert_eq!(
        iter.next().map(|(key, value)| (*key, *value)),
        Some((2, 20))
    );
}
