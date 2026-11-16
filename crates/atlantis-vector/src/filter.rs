//! Allow sets for filtered search.

/// The set of node slots a filtered search may return, as a bitmap.
#[derive(Debug, Clone, Default)]
pub struct AllowSet {
    bits: Vec<u64>,
    count: usize,
}

impl AllowSet {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty set with room for slots below `slots`.
    pub fn with_capacity(slots: usize) -> Self {
        AllowSet {
            bits: vec![0; slots.div_ceil(64)],
            count: 0,
        }
    }

    /// Adds `slot`; `true` if it was not in the set.
    pub fn insert(&mut self, slot: u32) -> bool {
        let (word, bit) = (slot as usize / 64, slot % 64);
        if word >= self.bits.len() {
            self.bits.resize(word + 1, 0);
        }
        let mask = 1u64 << bit;
        if self.bits[word] & mask != 0 {
            return false;
        }
        self.bits[word] |= mask;
        self.count += 1;
        true
    }

    /// `true` if `slot` is in the set.
    #[inline]
    pub fn contains(&self, slot: u32) -> bool {
        self.bits
            .get(slot as usize / 64)
            .is_some_and(|w| w & (1u64 << (slot % 64)) != 0)
    }

    /// The number of slots in the set.
    pub fn len(&self) -> usize {
        self.count
    }

    /// `true` if the set is empty.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The slots in ascending order.
    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        self.bits.iter().enumerate().flat_map(|(w, &word)| {
            let mut word = word;
            std::iter::from_fn(move || {
                if word == 0 {
                    return None;
                }
                let bit = word.trailing_zeros();
                word &= word - 1;
                Some(w as u32 * 64 + bit)
            })
        })
    }

    /// Keeps only the slots that are also in `other`.
    pub fn intersect(&mut self, other: &AllowSet) {
        self.bits.truncate(other.bits.len());
        let mut count = 0;
        for (a, b) in self.bits.iter_mut().zip(&other.bits) {
            *a &= b;
            count += a.count_ones() as usize;
        }
        self.count = count;
    }
}

impl FromIterator<u32> for AllowSet {
    fn from_iter<I: IntoIterator<Item = u32>>(iter: I) -> Self {
        let mut set = AllowSet::new();
        for slot in iter {
            set.insert(slot);
        }
        set
    }
}

/// How a filtered search finds its results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterStrategy {
    /// Exact distances to every allowed node: chosen when the allow set is
    /// small, where a graph walk would visit far more nodes than it keeps.
    Exact,
    /// Walk the whole graph (every node is a waypoint) but admit only
    /// allowed nodes to the results: chosen for broad filters.
    Traversal,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_contains_iter() {
        let mut s = AllowSet::with_capacity(10);
        assert!(s.insert(3));
        assert!(!s.insert(3));
        s.insert(64);
        s.insert(200);
        assert_eq!(s.len(), 3);
        assert!(s.contains(64) && s.contains(200) && !s.contains(65));
        assert_eq!(s.iter().collect::<Vec<_>>(), vec![3, 64, 200]);
    }

    #[test]
    fn intersect_counts() {
        let mut a: AllowSet = [1, 2, 3, 100, 300].into_iter().collect();
        let b: AllowSet = [2, 3, 100, 101].into_iter().collect();
        a.intersect(&b);
        assert_eq!(a.iter().collect::<Vec<_>>(), vec![2, 3, 100]);
        assert_eq!(a.len(), 3);
    }
}
