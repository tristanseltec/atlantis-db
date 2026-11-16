//! Per-thread search scratch: epoch-stamped visited sets and reusable heaps.

use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

use super::scored::Scored;

/// Visited marks for one search: slot `i` was visited iff
/// `marks[i] == epoch`. Starting a search bumps the epoch instead of
/// clearing the array.
#[derive(Default)]
pub(super) struct Visited {
    marks: Vec<u32>,
    epoch: u32,
}

impl Visited {
    pub(super) fn start(&mut self, slots: usize) {
        if self.marks.len() < slots {
            self.marks.resize(slots, 0);
        }
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.marks.fill(0);
            self.epoch = 1;
        }
    }

    /// Marks `i`; `true` if it was not marked yet.
    #[inline]
    pub(super) fn insert(&mut self, i: u32) -> bool {
        let m = &mut self.marks[i as usize];
        if *m == self.epoch {
            false
        } else {
            *m = self.epoch;
            true
        }
    }
}

/// Heaps of one layer search, reused per thread so a search does not
/// allocate them for every layer it walks.
#[derive(Default)]
pub(super) struct Heaps {
    /// Min-heap of nodes to expand.
    pub(super) candidates: BinaryHeap<Reverse<Scored>>,
    /// Max-heap of the best nodes found.
    pub(super) results: BinaryHeap<Scored>,
}

impl Heaps {
    /// Empties both heaps and makes room for `candidates` and `results`
    /// entries.
    pub(super) fn reset(&mut self, candidates: usize, results: usize) {
        self.candidates.clear();
        self.results.clear();
        self.candidates.reserve(candidates);
        self.results.reserve(results);
    }

    /// Drains the results, sorted closest first. `Scored` is a total order
    /// and the visited set admits each id once, so this is the order
    /// `into_sorted_vec` gives, while the heap keeps its buffer.
    pub(super) fn sorted_results(&mut self) -> Vec<Scored> {
        let mut out: Vec<Scored> = self.results.drain().collect();
        out.sort_unstable();
        out
    }
}

/// Everything one layer search needs besides the graph, kept per thread.
#[derive(Default)]
pub(super) struct Scratch {
    pub(super) visited: Visited,
    pub(super) heaps: Heaps,
}

thread_local! {
    pub(super) static SCRATCH: RefCell<Scratch> = RefCell::new(Scratch::default());
}
