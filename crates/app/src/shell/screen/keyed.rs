// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The one keyed collection the screens keep.
//!
//! Four of these were hand-rolled over a `Vec` of pairs: where this frame
//! painted each operable track, when each edited target is due, which rows are
//! open on the two screens that have them, and which curve node the keyboard is
//! on. They are the same object at a handful of entries, and four copies of an
//! upsert is four places for one of them to start pushing a duplicate key
//! instead of replacing the entry that was already there.
//!
//! A `Vec` rather than a hash map on purpose: nothing here ever holds more than
//! a device's worth of entries, so the linear scan costs less than hashing and
//! the order stays the order the screen produced.

/// One value held against each key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keyed<K, V>(Vec<(K, V)>);

/// The same collection carrying no value: a set of keys.
pub type Set<K> = Keyed<K, ()>;

impl<K, V> Default for Keyed<K, V> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<K: Copy + PartialEq, V> Keyed<K, V> {
    /// Replace what `key` holds, or record it for the first time.
    pub fn set(&mut self, key: K, value: V) {
        match self.0.iter_mut().find(|(known, _)| *known == key) {
            Some(entry) => entry.1 = value,
            None => self.0.push((key, value)),
        }
    }

    pub fn get(&self, key: K) -> Option<&V> {
        self.0
            .iter()
            .find(|(known, _)| *known == key)
            .map(|(_, value)| value)
    }

    pub fn contains(&self, key: K) -> bool {
        self.get(key).is_some()
    }

    pub fn remove(&mut self, key: K) {
        self.0.retain(|(known, _)| *known != key);
    }

    /// Forget every entry, so a row that went away takes its own with it.
    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn entries(&self) -> impl Iterator<Item = (K, &V)> {
        self.0.iter().map(|(key, value)| (*key, value))
    }

    /// Remove and return every key whose value satisfies `ready`.
    pub fn take(&mut self, ready: impl Fn(&V) -> bool) -> Vec<K> {
        let (taken, kept) = std::mem::take(&mut self.0)
            .into_iter()
            .partition::<Vec<_>, _>(|(_, value)| ready(value));
        self.0 = kept;
        taken.into_iter().map(|(key, _)| key).collect()
    }
}

impl<K: Copy + PartialEq> Set<K> {
    /// Add `key`, or drop it when it is already there.
    pub fn toggle(&mut self, key: K) {
        if self.contains(key) {
            self.remove(key);
        } else {
            self.set(key, ());
        }
    }

    pub fn insert(&mut self, key: K) {
        self.set(key, ());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_replaced_rather_than_duplicated() {
        let mut book: Keyed<u8, &str> = Keyed::default();
        book.set(1, "first");
        book.set(2, "second");
        book.set(1, "moved");

        assert_eq!(book.get(1), Some(&"moved"));
        assert_eq!(book.entries().count(), 2, "a key must not accumulate");

        book.remove(1);
        assert_eq!(book.get(1), None);
        book.clear();
        assert_eq!(book.entries().count(), 0);
    }

    #[test]
    fn taking_leaves_every_entry_that_was_not_ready() {
        let mut book: Keyed<u8, u32> = Keyed::default();
        for (key, value) in [(1, 10), (2, 20), (3, 30)] {
            book.set(key, value);
        }

        let mut taken = book.take(|value| *value <= 20);
        taken.sort_unstable();
        assert_eq!(taken, vec![1, 2]);
        assert!(!book.contains(1));
        assert_eq!(book.get(3), Some(&30));
    }

    #[test]
    fn a_set_toggles_a_key_in_and_out() {
        let mut open: Set<char> = Set::default();
        assert!(!open.contains('a'));

        open.toggle('a');
        open.toggle('b');
        assert!(open.contains('a') && open.contains('b'));

        open.toggle('a');
        assert!(!open.contains('a'), "a second toggle closes it");
        assert!(open.contains('b'), "and leaves the other alone");

        // Inserting twice is not toggling: the panel row opens by default and
        // must stay open when the same key is seeded again.
        open.insert('b');
        assert!(open.contains('b'));
    }
}
