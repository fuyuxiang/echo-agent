use std::collections::hash_map::Entry as StdEntry;
use std::collections::HashMap;
use std::hash::Hash;

/// A custom `OrderedHashMap` struct that maintains the order of keys.
/// It wraps a `Vec` to store keys and a `HashMap` to store key-value pairs.
#[derive(Clone, Debug, PartialEq)]
pub struct OrderedHashMap<K: Eq + Hash + Clone, V> {
    pub keys: Vec<K>,
    pub map: HashMap<K, V>,
}

impl<K: Eq + Hash + Clone, V> OrderedHashMap<K, V> {
    /// Creates a new empty `OrderedHashMap`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ordered_hashmap::OrderedHashMap;
    /// let ordered_map: OrderedHashMap<String, i32> = OrderedHashMap::new();
    /// ```
    pub fn new() -> Self {
        OrderedHashMap {
            keys: Vec::new(),
            map: HashMap::new(),
        }
    }

    /// Returns `true` if the map contains a value for the specified key.
    ///
    /// # Examples
    ///
    /// ```
    /// use ordered_hashmap::OrderedHashMap;
    /// let mut ordered_map = OrderedHashMap::new();
    /// ordered_map.insert("key1".to_string(), 42);
    /// assert!(ordered_map.contains_key(&"key1".to_string()));
    /// ```
    pub fn contains_key(&self, k: &K) -> bool {
        self.map.contains_key(k)
    }

    /// Inserts a key-value pair into the map. If the map did not have this key present, `None` is returned.
    /// If the map did have this key present, the value is updated, and the old value is returned.
    ///
    /// # Examples
    ///
    /// ```
    /// use ordered_hashmap::OrderedHashMap;
    /// let mut ordered_map = OrderedHashMap::new();
    /// assert_eq!(ordered_map.insert("key1".to_string(), 42), None);
    /// assert_eq!(ordered_map.insert("key1".to_string(), 99), Some(42));
    /// ```
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        if !self.map.contains_key(&key) {
            self.keys.push(key.clone());
        }
        self.map.insert(key, value)
    }

    /// Removes a key from the map, returning the value at the key if the key was previously in the map.
    ///
    /// # Examples
    ///
    /// ```
    /// use ordered_hashmap::OrderedHashMap;
    /// let mut ordered_map = OrderedHashMap::new();
    /// ordered_map.insert("key1".to_string(), 42);
    /// assert_eq!(ordered_map.remove(&"key1".to_string()), Some(42));
    /// ```
    pub fn remove(&mut self, key: &K) -> Option<V> {
        self.keys.retain(|k| k != key);
        self.map.remove(key)
    }

    /// Gets the value of the specified key.
    ///
    /// # Examples
    ///
    /// ```
    /// use ordered_hashmap::OrderedHashMap;
    /// let mut ordered_map = OrderedHashMap::new();
    /// ordered_map.insert("key1".to_string(), 42);
    /// assert_eq!(ordered_map.get(&"key1".to_string()), Some(&42));
    /// ```
    pub fn get(&self, key: &K) -> Option<&V> {
        self.map.get(key)
    }

    /// Gets a mutable reference to the value of the specified key.
    //////

    /// # Examples
    ///
    /// ```
    /// use ordered_hashmap::OrderedHashMap;
    /// let mut ordered_map = OrderedHashMap::new();
    /// ordered_map.insert("key1".to_string(), 42);
    /// *ordered_map.get_mut(&"key1".to_string()).unwrap() += 1;
    /// assert_eq!(ordered_map.get(&"key1".to_string()), Some(&43));
    /// ```
    pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        self.map.get_mut(key)
    }

    /// Returns an iterator over the values in the ordered hash map.
    ///
    /// # Examples
    ///
    /// ```
    /// use ordered_hashmap::OrderedHashMap;
    /// let mut ordered_map = OrderedHashMap::new();
    /// ordered_map.insert("key1".to_string(), 42);
    /// ordered_map.insert("key2".to_string(), 24);
    /// let values: Vec<_> = ordered_map.values().collect();
    /// assert_eq!(values, vec![&42, &24]);
    /// ```
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.keys.iter().filter_map(|k| self.map.get(k))
    }

    /// Returns a mutable iterator over the values in the ordered hash map.
    ///
    /// # Examples
    ///
    /// ```
    /// use ordered_hashmap::OrderedHashMap;
    /// let mut ordered_map = OrderedHashMap::new();
    /// ordered_map.insert("key1".to_string(), 42);
    /// ordered_map.insert("key2".to_string(), 24);
    /// ordered_map.values_mut().for_each(|value| *value += 1);
    /// assert_eq!(ordered_map.get(&"key1".to_string()), Some(&43));
    /// assert_eq!(ordered_map.get(&"key2".to_string()), Some(&25));
    /// ```
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut V> {
        let map_ptr = &mut self.map as *mut HashMap<K, V>;
        self.keys
            .iter()
            .filter_map(move |k| unsafe { (*map_ptr).get_mut(k) })
    }

    /// Returns an iterator over the keys in the ordered hash map.
    ///
    /// # Examples
    ///
    /// ```
    /// use ordered_hashmap::OrderedHashMap;
    /// let mut ordered_map = OrderedHashMap::new();
    /// ordered_map.insert("key1".to_string(), 42);
    /// ordered_map.insert("key2".to_string(), 24);
    /// let keys: Vec<_> = ordered_map.keys().collect();
    /// assert_eq!(keys, vec![&"key1".to_string(), &"key2".to_string()]);
    /// ```
    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.keys.iter()
    }

    /// Returns an iterator over the key-value pairs in the ordered hash map.
    ///
    /// # Examples
    ///
    /// ```
    /// use ordered_hashmap::OrderedHashMap;
    /// let mut ordered_map = OrderedHashMap::new();
    /// ordered_map.insert("key1".to_string(), 42);
    /// ordered_map.insert("key2".to_string(), 24);
    /// let pairs: Vec<_> = ordered_map.iter().collect();
    /// assert_eq!(pairs, vec![(&"key1".to_string(), &42), (&"key2".to_string(), &24)]);
    /// ```
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.keys
            .iter()
            .filter_map(move |k| self.map.get(k).map(|v| (k, v)))
    }

    /// Returns an `Entry` for the given key, allowing for more complex manipulation of the stored values.
    ///
    /// # Examples
    ///
    /// ```
    /// use ordered_hashmap::OrderedHashMap;
    /// let mut ordered_map = OrderedHashMap::new();
    /// ordered_map.insert("key1".to_string(), 42);
    /// ordered_map.entry("key1".to_string()).or_insert(99);
    /// assert_eq!(ordered_map.get(&"key1".to_string()), Some(&42));
    /// ```
    pub fn entry(&mut self, key: K) -> Entry<K, V> {
        match self.map.entry(key.clone()) {
            StdEntry::Occupied(occupied) => Entry::Occupied(occupied),
            StdEntry::Vacant(vacant) => {
                self.keys.push(key.clone());
                Entry::Vacant(vacant)
            }
        }
    }

    /// Returns a mutable iterator over the key-value pairs in the ordered hash map.
    ///
    /// # Examples
    ///
    /// ```
    /// use ordered_hashmap::OrderedHashMap;
    /// let mut ordered_map = OrderedHashMap::new();
    /// ordered_map.insert("key1".to_string(), 42);
    /// ordered_map.insert("key2".to_string(), 24);
    /// ordered_map.iter_mut().for_each(|(key, value)| *value += 1);
    /// assert_eq!(ordered_map.get(&"key1".to_string()), Some(&43));
    /// assert_eq!(ordered_map.get(&"key2".to_string()), Some(&25));
    /// ```
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&K, &mut V)> {
        let map_ptr = &mut self.map as *mut HashMap<K, V>;
        self.keys
            .iter()
            .filter_map(move |k| unsafe { (*map_ptr).get_mut(k).map(|v| (k, v)) })
    }

    /// Returns a vector of the values in the map, in the order corresponding to their keys.
    ///
    /// # Examples
    ///
    /// ```
    /// use ordered_hashmap::OrderedHashMap;
    ///
    /// let mut map = OrderedHashMap::new();
    /// map.insert(1, "one");
    /// map.insert(2, "two");
    /// map.insert(3, "three");
    ///
    /// let values = map.into_values();
    /// assert_eq!(values, vec!["one", "two", "three"]);
    /// ```
    pub fn into_values(self) -> Vec<V> {
        let mut extracted_items: Vec<(K, V)> = self.map.into_iter().collect();
        self.keys
            .into_iter()
            .filter_map(move |k| {
                if let Some(index) = extracted_items.iter().position(|(key, _)| *key == k) {
                    Some(extracted_items.remove(index).1)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
    }

    /// Adds all key-value pairs from another `OrderedHashMap` to this one, without replacing any existing pairs.
    ///
    /// # Examples
    ///
    /// ```
    /// use ordered_hashmap::OrderedHashMap;
    ///
    /// let mut map1 = OrderedHashMap::new();
    /// map1.insert(1, "one");
    ///
    /// let mut map2 = OrderedHashMap::new();
    /// map2.insert(2, "two");
    ///
    /// map1.extend(map2);
    ///
    /// assert_eq!(map1.get(&1), Some(&"one"));
    /// assert_eq!(map1.get(&2), Some(&"two"));
    /// ```
    pub fn extend(&mut self, slice: OrderedHashMap<K, V>) {
        slice.keys.into_iter().for_each(|k| {
            if !self.keys.contains(&k) {
                self.keys.push(k);
            }
        });
        self.map.extend(slice.map);
    }

    /// Returns the number of key-value pairs in the map.
    ///
    /// # Examples
    ///
    /// ```
    /// use ordered_hashmap::OrderedHashMap;
    ///
    /// let mut map = OrderedHashMap::new();
    /// assert_eq!(map.len(), 0);
    ///
    /// map.insert(1, "one");
    /// assert_eq!(map.len(), 1);
    ///
    /// map.insert(2, "two");
    /// assert_eq!(map.len(), 2);
    /// ```
    pub fn len(&self) -> usize {
        self.map.len()
    }
}

pub enum Entry<'a, K: 'a, V: 'a> {
    Occupied(std::collections::hash_map::OccupiedEntry<'a, K, V>),
    Vacant(std::collections::hash_map::VacantEntry<'a, K, V>),
}

impl<'a, K: Eq + Hash, V> Entry<'a, K, V> {
    pub fn or_insert(self, default: V) -> &'a mut V {
        match self {
            Entry::Occupied(occupied) => occupied.into_mut(),
            Entry::Vacant(vacant) => vacant.insert(default),
        }
    }

    pub fn or_insert_with<F: FnOnce() -> V>(self, default: F) -> &'a mut V {
        match self {
            Entry::Occupied(occupied) => occupied.into_mut(),
            Entry::Vacant(vacant) => vacant.insert(default()),
        }
    }
}

impl<K: Eq + Hash + Clone, V> IntoIterator for OrderedHashMap<K, V> {
    type Item = (K, V);
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        let mut extracted_items: Vec<(K, V)> = self.map.into_iter().collect();
        self.keys
            .into_iter()
            .filter_map(move |k| {
                if let Some(index) = extracted_items.iter().position(|(key, _)| *key == k) {
                    Some(extracted_items.remove(index))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .into_iter()
    }
}
