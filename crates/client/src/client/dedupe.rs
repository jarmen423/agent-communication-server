//! Bounded set of recently seen `meta.id`s (at-least-once → handled once).

use std::collections::{HashSet, VecDeque};

/// How many envelope ids an inbox remembers by default.
pub const DEFAULT_SEEN_CAPACITY: usize = 4096;

/// FIFO-evicting set of envelope ids. A redelivered envelope (same
/// `meta.id`) inside the window is reported as already seen.
#[derive(Debug)]
pub struct SeenIds {
    cap: usize,
    set: HashSet<String>,
    order: VecDeque<String>,
}

impl Default for SeenIds {
    fn default() -> Self {
        Self::new(DEFAULT_SEEN_CAPACITY)
    }
}

impl SeenIds {
    pub fn new(cap: usize) -> Self {
        let cap = cap.max(1);
        Self {
            cap,
            set: HashSet::with_capacity(cap),
            order: VecDeque::with_capacity(cap),
        }
    }

    /// Record `id`; `true` the first time, `false` for a duplicate.
    pub fn insert(&mut self, id: &str) -> bool {
        if self.set.contains(id) {
            return false;
        }
        if self.order.len() == self.cap {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        self.set.insert(id.to_string());
        self.order.push_back(id.to_string());
        true
    }

    pub fn contains(&self, id: &str) -> bool {
        self.set.contains(id)
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicates_are_reported_and_window_is_bounded() {
        let mut s = SeenIds::new(2);
        assert!(s.insert("a"));
        assert!(!s.insert("a"));
        assert!(s.insert("b"));
        assert!(s.insert("c")); // evicts "a"
        assert_eq!(s.len(), 2);
        assert!(!s.contains("a"));
        assert!(s.insert("a"));
    }
}
