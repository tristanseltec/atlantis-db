//! Deterministic synthetic corpus for the Retrieval Bench.
//!
//! The corpus models agent memory / GraphRAG data:
//!
//! - `chunks` text chunks, each with a unit-length embedding drawn from a
//!   mixture of low-dimensional Gaussian clusters (so nearest neighbours are
//!   meaningful, as with real text embeddings), a `user`
//!   (the filter attribute; `1 / users` selectivity, independent of the
//!   embedding, which is the hard case for filtered search) and synthetic
//!   text with topic words of its cluster (for BM25 once it exists);
//! - `docs = chunks / 10` documents; every chunk is `PART_OF` one document;
//! - `entities = chunks / 20` entities; every chunk `MENTIONS` one entity and
//!   every entity is `RELATED` to four others.
//!
//! With the defaults that is about 2.2 edges per chunk (2.2M at 1M chunks).
//! Every value comes from a seeded SplitMix64 stream, so a given
//! configuration produces the same corpus on every machine and run.

use serde::Serialize;

/// Corpus parameters.
#[derive(Clone, Debug, Serialize)]
pub struct Spec {
    pub chunks: usize,
    pub dim: usize,
    pub users: u32,
    pub clusters: usize,
    pub queries: usize,
    pub seed: u64,
}

impl Spec {
    /// Named presets. `tiny` is for smoke tests; `full` is the published scale.
    pub fn preset(name: &str) -> Option<Spec> {
        let (chunks, dim, queries) = match name {
            "tiny" => (2_000, 64, 100),
            "small" => (20_000, 768, 300),
            "medium" => (100_000, 768, 500),
            "full" => (1_000_000, 768, 1_000),
            _ => return None,
        };
        Some(Spec {
            chunks,
            dim,
            users: 100,
            clusters: 256,
            queries,
            seed: 0x00A7_1A57,
        })
    }

    pub fn docs(&self) -> usize {
        (self.chunks / 10).max(1)
    }

    pub fn entities(&self) -> usize {
        (self.chunks / 20).max(5)
    }
}

/// One query: a vector near a cluster centre, the user it is filtered to and
/// keyword text from the same cluster.
#[derive(Clone, Debug)]
pub struct Query {
    pub vector: Vec<f32>,
    pub user: u32,
    /// Keywords for the BM25 half of the hybrid query; unused until an
    /// adapter supports full-text search.
    #[allow(dead_code)]
    pub text: String,
}

/// The generated corpus. Ids are dense: chunk `i`, doc `i`, entity `i`.
pub struct Dataset {
    pub spec: Spec,
    /// Row-major `chunks × dim`, every row unit length.
    pub vectors: Vec<f32>,
    pub user: Vec<u32>,
    pub text: Vec<String>,
    /// `doc_of[chunk]`
    pub doc_of: Vec<u32>,
    /// `mentions[chunk]` (entity id)
    pub mentions: Vec<u32>,
    /// `(from, to)` entity pairs.
    pub related: Vec<(u32, u32)>,
    pub queries: Vec<Query>,
}

impl Dataset {
    pub fn vector(&self, chunk: usize) -> &[f32] {
        let d = self.spec.dim;
        &self.vectors[chunk * d..(chunk + 1) * d]
    }

    pub fn edge_count(&self) -> usize {
        self.doc_of.len() + self.mentions.len() + self.related.len()
    }

    pub fn generate(spec: Spec) -> Dataset {
        let mut rng = Rng::new(spec.seed);
        let d = spec.dim;

        let centres: Vec<Vec<f32>> = (0..spec.clusters)
            .map(|_| unit((0..d).map(|_| rng.gauss()).collect()))
            .collect();
        // Real text embeddings have a low intrinsic dimension: points of one
        // topic vary along a few directions, not all `dim` of them. Isotropic
        // noise in 768 dimensions instead makes every point of a cluster
        // nearly equidistant, which no approximate index can rank. So each
        // cluster gets `LOCAL_DIM` random directions carrying most of the
        // spread, plus a little isotropic noise.
        let bases: Vec<Vec<Vec<f32>>> = (0..spec.clusters)
            .map(|_| {
                (0..LOCAL_DIM)
                    .map(|_| unit((0..d).map(|_| rng.gauss()).collect()))
                    .collect()
            })
            .collect();
        let sample = |rng: &mut Rng, c: usize| -> Vec<f32> {
            let mut v = centres[c].clone();
            for b in &bases[c] {
                let a = LOCAL_SPREAD / (LOCAL_DIM as f32).sqrt() * rng.gauss();
                v.iter_mut().zip(b).for_each(|(x, y)| *x += a * y);
            }
            let sigma = NOISE / (d as f32).sqrt();
            v.iter_mut().for_each(|x| *x += sigma * rng.gauss());
            unit(v)
        };

        let mut vectors = Vec::with_capacity(spec.chunks * d);
        let mut user = Vec::with_capacity(spec.chunks);
        let mut text = Vec::with_capacity(spec.chunks);
        let mut doc_of = Vec::with_capacity(spec.chunks);
        let mut mentions = Vec::with_capacity(spec.chunks);
        let (docs, entities) = (spec.docs(), spec.entities());
        for _ in 0..spec.chunks {
            let c = rng.below(spec.clusters as u64) as usize;
            vectors.extend(sample(&mut rng, c));
            user.push(rng.below(spec.users as u64) as u32);
            text.push(chunk_text(&mut rng, c));
            doc_of.push(rng.below(docs as u64) as u32);
            mentions.push(rng.below(entities as u64) as u32);
        }
        let mut related = Vec::with_capacity(entities * 4);
        for e in 0..entities as u32 {
            for _ in 0..4 {
                let mut o = rng.below(entities as u64) as u32;
                if o == e {
                    o = (o + 1) % entities as u32;
                }
                related.push((e, o));
            }
        }
        let queries = (0..spec.queries)
            .map(|_| {
                let c = rng.below(spec.clusters as u64) as usize;
                Query {
                    vector: sample(&mut rng, c),
                    user: rng.below(spec.users as u64) as u32,
                    text: (0..3)
                        .map(|_| topic_word(c, rng.below(TOPIC_WORDS)))
                        .collect::<Vec<_>>()
                        .join(" "),
                }
            })
            .collect();

        Dataset {
            spec,
            vectors,
            user,
            text,
            doc_of,
            mentions,
            related,
            queries,
        }
    }
}

/// Directions of variation within a cluster.
const LOCAL_DIM: usize = 16;
/// Norm of the within-cluster variation (the centre has norm 1).
const LOCAL_SPREAD: f32 = 0.7;
/// Norm of the isotropic noise.
const NOISE: f32 = 0.1;

const TOPIC_WORDS: u64 = 20;
const GENERAL_WORDS: u64 = 5_000;

fn topic_word(cluster: usize, i: u64) -> String {
    format!("t{cluster}x{i}")
}

/// Six topic words of the chunk's cluster and six Zipf-distributed general words.
fn chunk_text(rng: &mut Rng, cluster: usize) -> String {
    let mut words = Vec::with_capacity(12);
    for _ in 0..6 {
        words.push(topic_word(cluster, rng.below(TOPIC_WORDS)));
        words.push(format!("w{}", rng.zipf(GENERAL_WORDS)));
    }
    words.join(" ")
}

fn unit(mut v: Vec<f32>) -> Vec<f32> {
    let n = v
        .iter()
        .map(|x| x * x)
        .sum::<f32>()
        .sqrt()
        .max(f32::MIN_POSITIVE);
    v.iter_mut().for_each(|x| *x /= n);
    v
}

/// SplitMix64 with Box–Muller Gaussians. Self-contained so the corpus never
/// changes with a dependency upgrade.
pub struct Rng {
    state: u64,
    spare: Option<f32>,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng {
            state: seed,
            spare: None,
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn below(&mut self, n: u64) -> u64 {
        ((self.next_u64() as u128 * n as u128) >> 64) as u64
    }

    pub fn gauss(&mut self) -> f32 {
        if let Some(s) = self.spare.take() {
            return s;
        }
        let u1 = self.unit().max(f64::MIN_POSITIVE);
        let u2 = self.unit();
        let r = (-2.0 * u1.ln()).sqrt();
        let t = std::f64::consts::TAU * u2;
        self.spare = Some((r * t.sin()) as f32);
        (r * t.cos()) as f32
    }

    /// Approximate Zipf(1) over `1..=n` by inverse transform on the harmonic CDF.
    pub fn zipf(&mut self, n: u64) -> u64 {
        let h = (n as f64).ln() + 0.5772;
        ((self.unit() * h).exp() as u64).clamp(1, n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_is_deterministic_and_well_formed() {
        let spec = Spec::preset("tiny").unwrap();
        let a = Dataset::generate(spec.clone());
        let b = Dataset::generate(spec.clone());
        assert_eq!(a.vectors, b.vectors);
        assert_eq!(a.text, b.text);
        assert_eq!(a.vectors.len(), spec.chunks * spec.dim);
        let norm: f32 = a.vector(7).iter().map(|x| x * x).sum();
        assert!((norm - 1.0).abs() < 1e-4);
        assert!(a.user.iter().all(|u| *u < spec.users));
        assert_eq!(a.related.len(), spec.entities() * 4);
        assert!(a.related.iter().all(|(f, t)| f != t));
    }
}
