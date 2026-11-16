//! Distance metrics for vector similarity.
//!
//! The hot loops are [`dot`] and [`l2_squared`]. On x86-64 they use AVX2 and
//! FMA when the CPU has them (detected at run time, so one binary runs
//! everywhere); otherwise a portable version with independent accumulators,
//! which the compiler vectorises with the baseline SIMD of the target.

use atlantis_core::DistanceMetricKind;

/// Trait for computing distance between two vectors.
pub trait DistanceMetric: Send + Sync {
    /// Returns the distance between vectors `a` and `b`.
    /// Lower distance = more similar.
    fn distance(&self, a: &[f32], b: &[f32]) -> f32;

    /// Returns the name of this metric.
    fn name(&self) -> &'static str;
}

/// Dot product of `a` and `b` (over the shorter of the two).
#[inline]
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let (a, b) = (&a[..n], &b[..n]);
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("avx2") && std::arch::is_x86_feature_detected!("fma")
        {
            // SAFETY: the CPU supports AVX2 and FMA (checked above), and
            // both slices have length `n`.
            return unsafe { x86::dot(a, b) };
        }
    }
    portable::dot(a, b)
}

/// Squared Euclidean distance between `a` and `b` (over the shorter of the two).
#[inline]
pub fn l2_squared(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let (a, b) = (&a[..n], &b[..n]);
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("avx2") && std::arch::is_x86_feature_detected!("fma")
        {
            // SAFETY: as in `dot`.
            return unsafe { x86::l2_squared(a, b) };
        }
    }
    portable::l2_squared(a, b)
}

/// `1 / ‖v‖`, or `0` for the zero vector (which then has cosine distance 1
/// to everything).
#[inline]
pub fn inverse_norm(v: &[f32]) -> f32 {
    let n = dot(v, v).sqrt();
    if n > 0.0 && n.is_finite() {
        1.0 / n
    } else {
        0.0
    }
}

/// Cosine distance from a dot product and the two inverse norms.
#[inline]
pub fn cosine_from_dot(dot: f32, inv_a: f32, inv_b: f32) -> f32 {
    if inv_a == 0.0 || inv_b == 0.0 {
        return 1.0;
    }
    1.0 - (dot * inv_a * inv_b).clamp(-1.0, 1.0)
}

mod portable {
    const LANES: usize = 16;

    // Independent accumulators let the compiler keep LANES partial sums in
    // vector registers; a single running sum would serialise every add.
    pub fn dot(a: &[f32], b: &[f32]) -> f32 {
        let mut acc = [0.0f32; LANES];
        let ((ca, ra), (cb, rb)) = (a.as_chunks::<LANES>(), b.as_chunks::<LANES>());
        for (x, y) in ca.iter().zip(cb) {
            for i in 0..LANES {
                acc[i] += x[i] * y[i];
            }
        }
        let mut sum: f32 = acc.iter().sum();
        for (x, y) in ra.iter().zip(rb) {
            sum += x * y;
        }
        sum
    }

    pub fn l2_squared(a: &[f32], b: &[f32]) -> f32 {
        let mut acc = [0.0f32; LANES];
        let ((ca, ra), (cb, rb)) = (a.as_chunks::<LANES>(), b.as_chunks::<LANES>());
        for (x, y) in ca.iter().zip(cb) {
            for i in 0..LANES {
                let d = x[i] - y[i];
                acc[i] += d * d;
            }
        }
        let mut sum: f32 = acc.iter().sum();
        for (x, y) in ra.iter().zip(rb) {
            let d = x - y;
            sum += d * d;
        }
        sum
    }
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use std::arch::x86_64::*;

    #[target_feature(enable = "avx2,fma")]
    unsafe fn hsum(v: __m256) -> f32 {
        let lo = _mm256_castps256_ps128(v);
        let hi = _mm256_extractf128_ps(v, 1);
        let s = _mm_add_ps(lo, hi);
        let s = _mm_add_ps(s, _mm_movehl_ps(s, s));
        let s = _mm_add_ss(s, _mm_shuffle_ps(s, s, 0b01));
        _mm_cvtss_f32(s)
    }

    /// # Safety
    /// The CPU must support AVX2 and FMA, and `a.len() == b.len()`.
    #[target_feature(enable = "avx2,fma")]
    pub unsafe fn dot(a: &[f32], b: &[f32]) -> f32 {
        let n = a.len();
        let (pa, pb) = (a.as_ptr(), b.as_ptr());
        let mut acc = [_mm256_setzero_ps(); 4];
        let mut i = 0;
        while i + 32 <= n {
            for (j, s) in acc.iter_mut().enumerate() {
                let x = _mm256_loadu_ps(pa.add(i + 8 * j));
                let y = _mm256_loadu_ps(pb.add(i + 8 * j));
                *s = _mm256_fmadd_ps(x, y, *s);
            }
            i += 32;
        }
        while i + 8 <= n {
            let x = _mm256_loadu_ps(pa.add(i));
            let y = _mm256_loadu_ps(pb.add(i));
            acc[0] = _mm256_fmadd_ps(x, y, acc[0]);
            i += 8;
        }
        let s = _mm256_add_ps(_mm256_add_ps(acc[0], acc[1]), _mm256_add_ps(acc[2], acc[3]));
        let mut sum = hsum(s);
        while i < n {
            sum += *pa.add(i) * *pb.add(i);
            i += 1;
        }
        sum
    }

    /// # Safety
    /// The CPU must support AVX2 and FMA, and `a.len() == b.len()`.
    #[target_feature(enable = "avx2,fma")]
    pub unsafe fn l2_squared(a: &[f32], b: &[f32]) -> f32 {
        let n = a.len();
        let (pa, pb) = (a.as_ptr(), b.as_ptr());
        let mut acc = [_mm256_setzero_ps(); 4];
        let mut i = 0;
        while i + 32 <= n {
            for (j, s) in acc.iter_mut().enumerate() {
                let x = _mm256_loadu_ps(pa.add(i + 8 * j));
                let y = _mm256_loadu_ps(pb.add(i + 8 * j));
                let d = _mm256_sub_ps(x, y);
                *s = _mm256_fmadd_ps(d, d, *s);
            }
            i += 32;
        }
        while i + 8 <= n {
            let x = _mm256_loadu_ps(pa.add(i));
            let y = _mm256_loadu_ps(pb.add(i));
            let d = _mm256_sub_ps(x, y);
            acc[0] = _mm256_fmadd_ps(d, d, acc[0]);
            i += 8;
        }
        let s = _mm256_add_ps(_mm256_add_ps(acc[0], acc[1]), _mm256_add_ps(acc[2], acc[3]));
        let mut sum = hsum(s);
        while i < n {
            let d = *pa.add(i) - *pb.add(i);
            sum += d * d;
            i += 1;
        }
        sum
    }
}

/// Cosine distance = 1 - cosine similarity.
///
/// Vectors that are not normalised are handled by dividing by the product of
/// norms; a zero vector has distance 1 to everything.
#[derive(Debug, Clone, Copy, Default)]
pub struct CosineDistance;

impl DistanceMetric for CosineDistance {
    fn distance(&self, a: &[f32], b: &[f32]) -> f32 {
        cosine_from_dot(dot(a, b), inverse_norm(a), inverse_norm(b))
    }

    fn name(&self) -> &'static str {
        "cosine"
    }
}

/// Euclidean (L2) distance.
#[derive(Debug, Clone, Copy, Default)]
pub struct EuclideanDistance;

impl DistanceMetric for EuclideanDistance {
    fn distance(&self, a: &[f32], b: &[f32]) -> f32 {
        l2_squared(a, b).sqrt()
    }

    fn name(&self) -> &'static str {
        "euclidean"
    }
}

/// Dot product distance (negative dot product, so lower = more similar).
#[derive(Debug, Clone, Copy, Default)]
pub struct DotProductDistance;

impl DistanceMetric for DotProductDistance {
    fn distance(&self, a: &[f32], b: &[f32]) -> f32 {
        -dot(a, b)
    }

    fn name(&self) -> &'static str {
        "dot_product"
    }
}

/// Creates a boxed distance metric from the given kind.
pub fn from_kind(kind: DistanceMetricKind) -> Box<dyn DistanceMetric> {
    match kind {
        DistanceMetricKind::Cosine => Box::new(CosineDistance),
        DistanceMetricKind::Euclidean => Box::new(EuclideanDistance),
        DistanceMetricKind::DotProduct => Box::new(DotProductDistance),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive_dot(a: &[f32], b: &[f32]) -> f32 {
        a.iter()
            .zip(b)
            .map(|(x, y)| (*x as f64) * (*y as f64))
            .sum::<f64>() as f32
    }

    #[test]
    fn kernels_match_naive_for_every_length() {
        // Covers the 32-wide, 8-wide and scalar tails of the SIMD loops.
        for n in 0..100 {
            let a: Vec<f32> = (0..n).map(|i| ((i * 7 + 3) as f32).sin()).collect();
            let b: Vec<f32> = (0..n).map(|i| ((i * 5 + 1) as f32).cos()).collect();
            let d = dot(&a, &b);
            assert!((d - naive_dot(&a, &b)).abs() < 1e-4, "dot n={n}");
            assert!(
                (portable::dot(&a, &b) - d).abs() < 1e-4,
                "portable dot n={n}"
            );
            let l2: f32 = a.iter().zip(&b).map(|(x, y)| (x - y) * (x - y)).sum();
            assert!((l2_squared(&a, &b) - l2).abs() < 1e-3, "l2 n={n}");
            assert!(
                (portable::l2_squared(&a, &b) - l2).abs() < 1e-3,
                "portable l2 n={n}"
            );
        }
    }

    #[test]
    fn inverse_norm_of_zero_is_zero() {
        assert_eq!(inverse_norm(&[0.0, 0.0]), 0.0);
        assert!((inverse_norm(&[3.0, 4.0]) - 0.2).abs() < 1e-7);
        assert_eq!(cosine_from_dot(1.0, 0.0, 1.0), 1.0);
    }

    #[test]
    fn test_cosine_identical_vectors() {
        let metric = CosineDistance;
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        let dist = metric.distance(&a, &b);
        assert!(
            dist.abs() < 1e-6,
            "identical vectors should have ~0 distance, got {dist}"
        );
    }

    #[test]
    fn test_cosine_orthogonal_vectors() {
        let metric = CosineDistance;
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let dist = metric.distance(&a, &b);
        assert!(
            (dist - 1.0).abs() < 1e-6,
            "orthogonal vectors should have distance ~1, got {dist}"
        );
    }

    #[test]
    fn test_cosine_opposite_vectors() {
        let metric = CosineDistance;
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        let dist = metric.distance(&a, &b);
        assert!(
            (dist - 2.0).abs() < 1e-6,
            "opposite vectors should have distance ~2, got {dist}"
        );
    }

    #[test]
    fn test_cosine_zero_vector() {
        let metric = CosineDistance;
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 3.0];
        let dist = metric.distance(&a, &b);
        assert!(
            (dist - 1.0).abs() < 1e-6,
            "zero vector should have distance 1, got {dist}"
        );
    }

    #[test]
    fn test_euclidean_identical() {
        let metric = EuclideanDistance;
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0, 3.0];
        assert!(metric.distance(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn test_euclidean_basic() {
        let metric = EuclideanDistance;
        let a = vec![0.0, 0.0];
        let b = vec![3.0, 4.0];
        let dist = metric.distance(&a, &b);
        assert!(
            (dist - 5.0).abs() < 1e-6,
            "3-4-5 triangle should give distance 5, got {dist}"
        );
    }

    #[test]
    fn test_dot_product() {
        let metric = DotProductDistance;
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![4.0, 5.0, 6.0];
        let dist = metric.distance(&a, &b);
        // dot = 1*4 + 2*5 + 3*6 = 32, distance = -32
        assert!((dist - (-32.0)).abs() < 1e-5);
    }

    #[test]
    fn test_from_kind() {
        let cosine = from_kind(DistanceMetricKind::Cosine);
        assert_eq!(cosine.name(), "cosine");

        let euclidean = from_kind(DistanceMetricKind::Euclidean);
        assert_eq!(euclidean.name(), "euclidean");

        let dot = from_kind(DistanceMetricKind::DotProduct);
        assert_eq!(dot.name(), "dot_product");
    }

    #[test]
    fn test_cosine_45_degrees() {
        let metric = CosineDistance;
        let a = vec![1.0, 0.0];
        let b = vec![1.0, 1.0]; // 45 degrees
        let dist = metric.distance(&a, &b);
        // cos(45) = sqrt(2)/2 ≈ 0.707, distance = 1 - 0.707 ≈ 0.293
        let expected = 1.0 - (2.0_f32).sqrt() / 2.0;
        assert!(
            (dist - expected).abs() < 1e-5,
            "expected {expected}, got {dist}"
        );
    }
}
