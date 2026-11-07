//! Distance metrics for vector similarity.

use atlantis_core::DistanceMetricKind;

/// Trait for computing distance between two vectors.
pub trait DistanceMetric: Send + Sync {
    /// Returns the distance between vectors `a` and `b`.
    /// Lower distance = more similar.
    fn distance(&self, a: &[f32], b: &[f32]) -> f32;

    /// Returns the name of this metric.
    fn name(&self) -> &'static str;
}

/// Cosine distance = 1 - cosine similarity.
///
/// For normalized vectors, this equals 1 - dot(a, b).
/// Not normalized vectors are handled by dividing by the product of norms.
#[derive(Debug, Clone, Copy, Default)]
pub struct CosineDistance;

impl DistanceMetric for CosineDistance {
    fn distance(&self, a: &[f32], b: &[f32]) -> f32 {
        let dot = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum::<f32>();
        let norm_a = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let norm_b = b.iter().map(|x| x * x).sum::<f32>().sqrt();

        if norm_a == 0.0 || norm_b == 0.0 {
            return 1.0; // max distance for zero vectors
        }

        let similarity = dot / (norm_a * norm_b);
        // Clamp to [-1, 1] to handle floating point errors
        let clamped = similarity.clamp(-1.0, 1.0);
        1.0 - clamped
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
        a.iter()
            .zip(b.iter())
            .map(|(x, y)| (x - y).powi(2))
            .sum::<f32>()
            .sqrt()
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
        -a.iter().zip(b.iter()).map(|(x, y)| x * y).sum::<f32>()
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
