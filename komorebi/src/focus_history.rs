use std::collections::VecDeque;
use std::collections::vec_deque::Iter;

use serde::Deserialize;
use serde::Serialize;

/// A deduplicated most-recently-used ordering of stable keys.
///
/// The front of the list is the most recent entry. Recording an entry which is already present
/// moves it to the front instead of appending a duplicate, so an entry never appears twice.
///
/// This single representation backs the workspace container focus history, the container window
/// focus history, and the per-workspace minimize history, so recording, removal, selection, and
/// pruning behave identically for all three.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct Mru<T> {
    entries: VecDeque<T>,
}

impl<T> Default for Mru<T> {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
        }
    }
}

impl<T> Mru<T> {
    pub const fn entries(&self) -> &VecDeque<T> {
        &self.entries
    }

    pub fn iter(&self) -> Iter<'_, T> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn most_recent(&self) -> Option<&T> {
        self.entries.front()
    }

    pub fn oldest(&self) -> Option<&T> {
        self.entries.back()
    }

    /// Drop every entry which no longer refers to a live object.
    pub fn retain(&mut self, keep: impl FnMut(&T) -> bool) {
        self.entries.retain(keep);
    }

    /// Return the most recent entry which satisfies `is_valid`, without mutating the history.
    pub fn first_valid(&self, is_valid: impl Fn(&T) -> bool) -> Option<&T> {
        self.entries.iter().find(|entry| is_valid(entry))
    }
}

impl<T: PartialEq> Mru<T> {
    pub fn contains(&self, entry: &T) -> bool {
        self.entries.contains(entry)
    }

    /// Move `entry` to the most recent position, inserting it if it is not present yet.
    pub fn record(&mut self, entry: T) {
        self.entries.retain(|existing| existing != &entry);
        self.entries.push_front(entry);
    }

    /// Insert `entry` in the oldest position if it is not present. Used when repairing a history
    /// which is missing an object that still exists; it must never displace real focus order.
    pub fn record_oldest(&mut self, entry: T) {
        if !self.contains(&entry) {
            self.entries.push_back(entry);
        }
    }

    /// Remove `entry`, reporting whether the history actually referenced it.
    pub fn remove(&mut self, entry: &T) -> bool {
        let len = self.entries.len();
        self.entries.retain(|existing| existing != entry);
        len != self.entries.len()
    }

    /// Take the most recent valid entry, discarding every stale entry examined on the way.
    ///
    /// This is the single consumption path for the minimize history, where entries for windows
    /// which no longer exist must not survive a restore attempt.
    pub fn take_first_valid(&mut self, is_valid: impl Fn(&T) -> bool) -> Option<T> {
        while let Some(entry) = self.entries.pop_front() {
            if is_valid(&entry) {
                return Some(entry);
            }
        }

        None
    }
}

impl<T: PartialEq> FromIterator<T> for Mru<T> {
    /// Build a history from most recent to oldest, discarding duplicates after their first
    /// (most recent) occurrence.
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut mru = Self::default();

        for entry in iter {
            mru.record_oldest(entry);
        }

        mru
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_moves_an_existing_entry_to_the_front_without_duplicating_it() {
        let mut mru = Mru::default();

        mru.record(1);
        mru.record(2);
        mru.record(3);
        mru.record(1);

        assert_eq!(mru.iter().copied().collect::<Vec<_>>(), vec![1, 3, 2]);
        assert_eq!(mru.len(), 3);
        assert_eq!(mru.most_recent(), Some(&1));
        assert_eq!(mru.oldest(), Some(&2));
    }

    #[test]
    fn repairing_entries_never_displaces_real_focus_order() {
        let mut mru = Mru::from_iter([3, 2]);

        mru.record_oldest(1);
        mru.record_oldest(3);

        assert_eq!(mru.iter().copied().collect::<Vec<_>>(), vec![3, 2, 1]);
    }

    #[test]
    fn removal_reports_whether_the_history_referenced_the_entry() {
        let mut mru = Mru::from_iter([1, 2]);

        assert!(mru.remove(&1));
        assert!(!mru.remove(&1));
        assert_eq!(mru.iter().copied().collect::<Vec<_>>(), vec![2]);
    }

    #[test]
    fn selection_skips_invalid_entries_without_mutating_the_history() {
        let mru = Mru::from_iter([1, 2, 3]);

        assert_eq!(mru.first_valid(|entry| *entry > 1), Some(&2));
        assert_eq!(mru.len(), 3);
        assert_eq!(mru.first_valid(|entry| *entry > 9), None);
    }

    #[test]
    fn taking_a_valid_entry_discards_the_stale_entries_it_passed() {
        let mut mru = Mru::from_iter([1, 2, 3]);

        assert_eq!(mru.take_first_valid(|entry| *entry == 3), Some(3));
        assert!(mru.is_empty());
    }

    #[test]
    fn taking_from_an_entirely_stale_history_empties_it() {
        let mut mru = Mru::from_iter([1, 2]);

        assert_eq!(mru.take_first_valid(|_| false), None);
        assert!(mru.is_empty());
    }

    #[test]
    fn histories_serialize_as_plain_ordered_lists() {
        let mru = Mru::from_iter([3, 1]);
        let json = serde_json::to_string(&mru).unwrap();

        assert_eq!(json, "[3,1]");
        assert_eq!(serde_json::from_str::<Mru<i32>>(&json).unwrap(), mru);
    }
}
