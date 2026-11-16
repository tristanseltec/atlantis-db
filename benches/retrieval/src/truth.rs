//! Exact ground truth by brute force: for every query, the true top-k chunks
//! by cosine similarity (dot product of unit vectors), unfiltered and
//! filtered to the query's user.

use std::cmp::Ordering;

use crate::dataset::Dataset;

pub struct Truth {
    /// `unfiltered[q]`: chunk ids, best first.
    pub unfiltered: Vec<Vec<u64>>,
    /// `filtered[q]`: chunk ids of the query's user, best first.
    pub filtered: Vec<Vec<u64>>,
}

impl Truth {
    pub fn compute(ds: &Dataset, k: usize, threads: usize) -> Truth {
        let n = ds.queries.len();
        let mut unfiltered = vec![Vec::new(); n];
        let mut filtered = vec![Vec::new(); n];
        let per = n.div_ceil(threads.max(1)).max(1);
        std::thread::scope(|s| {
            for (i, (u, f)) in unfiltered
                .chunks_mut(per)
                .zip(filtered.chunks_mut(per))
                .enumerate()
            {
                s.spawn(move || {
                    for (j, (u, f)) in u.iter_mut().zip(f.iter_mut()).enumerate() {
                        let q = &ds.queries[i * per + j];
                        let (a, b) = exact(ds, &q.vector, q.user, k);
                        *u = a;
                        *f = b;
                    }
                });
            }
        });
        Truth {
            unfiltered,
            filtered,
        }
    }
}

fn exact(ds: &Dataset, q: &[f32], user: u32, k: usize) -> (Vec<u64>, Vec<u64>) {
    let mut all = TopK::new(k);
    let mut mine = TopK::new(k);
    for c in 0..ds.spec.chunks {
        let s = dot(q, ds.vector(c));
        all.push(s, c as u64);
        if ds.user[c] == user {
            mine.push(s, c as u64);
        }
    }
    (all.into_ids(), mine.into_ids())
}

pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    // Eight independent accumulators let the compiler vectorize the loop.
    let mut acc = [0f32; 8];
    let ((ca, ra), (cb, rb)) = (a.as_chunks::<8>(), b.as_chunks::<8>());
    for (x, y) in ca.iter().zip(cb) {
        for i in 0..8 {
            acc[i] += x[i] * y[i];
        }
    }
    acc.iter().sum::<f32>() + ra.iter().zip(rb).map(|(x, y)| x * y).sum::<f32>()
}

/// Keeps the `k` highest scores (ties broken by lower id).
struct TopK {
    k: usize,
    items: Vec<(f32, u64)>,
}

impl TopK {
    fn new(k: usize) -> Self {
        TopK {
            k,
            items: Vec::with_capacity(k + 1),
        }
    }

    fn push(&mut self, score: f32, id: u64) {
        if self.items.len() == self.k {
            let worst = self.items[self.k - 1];
            if better(worst, (score, id)) != Ordering::Greater {
                return;
            }
        }
        let pos = self
            .items
            .partition_point(|&x| better(x, (score, id)) == Ordering::Less);
        self.items.insert(pos, (score, id));
        self.items.truncate(self.k);
    }

    fn into_ids(self) -> Vec<u64> {
        self.items.into_iter().map(|(_, id)| id).collect()
    }
}

/// `Less` when `a` ranks before `b`.
fn better(a: (f32, u64), b: (f32, u64)) -> Ordering {
    b.0.partial_cmp(&a.0)
        .unwrap_or(Ordering::Equal)
        .then(a.1.cmp(&b.1))
}

/// recall@k of `got` against `truth` (|got ∩ truth| / |truth|).
pub fn recall(got: &[u64], truth: &[u64]) -> f64 {
    if truth.is_empty() {
        return 1.0;
    }
    let hit = truth.iter().filter(|t| got.contains(t)).count();
    hit as f64 / truth.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dataset::Spec;

    #[test]
    fn topk_keeps_the_best_in_order() {
        let mut t = TopK::new(3);
        for (s, id) in [(0.1, 1), (0.9, 2), (0.5, 3), (0.7, 4), (0.9, 0)] {
            t.push(s, id);
        }
        assert_eq!(t.into_ids(), vec![0, 2, 4]);
    }

    #[test]
    fn filtered_truth_belongs_to_the_user() {
        let ds = Dataset::generate(Spec::preset("tiny").unwrap());
        let truth = Truth::compute(&ds, 10, 4);
        for (q, ids) in ds.queries.iter().zip(&truth.filtered) {
            assert!(ids.iter().all(|&c| ds.user[c as usize] == q.user));
        }
        assert!(truth.unfiltered.iter().all(|ids| ids.len() == 10));
    }
}
