//! Result fusion algorithms for hybrid search
//!
//! Implements Reciprocal Rank Fusion (RRF) for combining results from
//! multiple ranking systems (e.g., vector search + full-text search).

use std::collections::HashMap;

/// Reciprocal Rank Fusion (RRF)
///
/// Combines multiple ranked lists using the RRF algorithm:
/// score(d) = Σ(weight_i / (k + rank_i(d)))
///
/// # Arguments
/// * `vector_results` - Results from vector search (id, score)
/// * `fulltext_results` - Results from full-text search (id, score)
/// * `vector_weight` - Weight for vector search (default: 0.5)
/// * `fulltext_weight` - Weight for full-text search (default: 0.5)
/// * `k` - RRF constant (default: 60, as per literature)
/// * `top_k` - Number of results to return
///
/// # Returns
/// Combined results sorted by RRF score (id, rrf_score)
///
/// # References
/// - Cormack, Clarke, and Buettcher. "Reciprocal Rank Fusion Outperforms Condorcet
///   and Individual Rank Learning Methods." SIGIR 2009.
pub fn reciprocal_rank_fusion(
    vector_results: Option<&[(u64, f32)]>,
    fulltext_results: Option<&[(u64, f32)]>,
    vector_weight: f32,
    fulltext_weight: f32,
    k: f32,
    top_k: usize,
) -> Vec<(u64, f32)> {
    let mut rrf_scores: HashMap<u64, f32> = HashMap::new();

    // Process vector search results
    if let Some(results) = vector_results {
        for (rank, (doc_id, _original_score)) in results.iter().enumerate() {
            let rank_score = vector_weight / (k + (rank as f32 + 1.0));
            *rrf_scores.entry(*doc_id).or_insert(0.0) += rank_score;
        }
    }

    // Process full-text search results
    if let Some(results) = fulltext_results {
        for (rank, (doc_id, _original_score)) in results.iter().enumerate() {
            let rank_score = fulltext_weight / (k + (rank as f32 + 1.0));
            *rrf_scores.entry(*doc_id).or_insert(0.0) += rank_score;
        }
    }

    // Sort by RRF score (descending)
    let mut results: Vec<_> = rrf_scores.into_iter().collect();
    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Take top_k
    results.truncate(top_k);
    results
}

/// Simple weighted score fusion
///
/// Combines results by averaging the original scores with weights.
/// Simpler alternative to RRF when you want to preserve score magnitudes.
///
/// # Arguments
/// * `vector_results` - Results from vector search (id, score)
/// * `fulltext_results` - Results from full-text search (id, score)
/// * `vector_weight` - Weight for vector scores
/// * `fulltext_weight` - Weight for full-text scores
/// * `top_k` - Number of results to return
pub fn weighted_score_fusion(
    vector_results: Option<&[(u64, f32)]>,
    fulltext_results: Option<&[(u64, f32)]>,
    vector_weight: f32,
    fulltext_weight: f32,
    top_k: usize,
) -> Vec<(u64, f32)> {
    let mut combined_scores: HashMap<u64, (f32, usize)> = HashMap::new();

    // Add vector scores
    if let Some(results) = vector_results {
        for (doc_id, score) in results {
            let entry = combined_scores.entry(*doc_id).or_insert((0.0, 0));
            entry.0 += score * vector_weight;
            entry.1 += 1;
        }
    }

    // Add full-text scores
    if let Some(results) = fulltext_results {
        for (doc_id, score) in results {
            let entry = combined_scores.entry(*doc_id).or_insert((0.0, 0));
            entry.0 += score * fulltext_weight;
            entry.1 += 1;
        }
    }

    // Average scores by number of sources
    let mut results: Vec<_> = combined_scores
        .into_iter()
        .map(|(id, (sum, count))| (id, sum / count as f32))
        .collect();

    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(top_k);
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rrf_both_results() {
        let vector_results = vec![(1, 0.9), (2, 0.8), (3, 0.7)];
        let fulltext_results = vec![(2, 10.0), (3, 8.0), (4, 6.0)];

        let merged = reciprocal_rank_fusion(
            Some(&vector_results),
            Some(&fulltext_results),
            0.5, // equal weights
            0.5,
            60.0,
            10,
        );

        // Doc 2 and 3 appear in both, should rank higher
        assert!(merged.len() >= 2);
        let top_ids: Vec<u64> = merged.iter().map(|(id, _)| *id).collect();
        assert!(top_ids.contains(&2) || top_ids.contains(&3));
    }

    #[test]
    fn test_rrf_vector_only() {
        let vector_results = vec![(1, 0.9), (2, 0.8), (3, 0.7)];

        let merged = reciprocal_rank_fusion(Some(&vector_results), None, 0.5, 0.5, 60.0, 10);

        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].0, 1); // Highest rank
    }

    #[test]
    fn test_rrf_fulltext_only() {
        let fulltext_results = vec![(1, 10.0), (2, 8.0), (3, 6.0)];

        let merged = reciprocal_rank_fusion(None, Some(&fulltext_results), 0.5, 0.5, 60.0, 10);

        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].0, 1); // Highest rank
    }

    #[test]
    fn test_rrf_weights() {
        let vector_results = vec![(1, 0.9)];
        let fulltext_results = vec![(2, 10.0)];

        // Heavily favor vector search
        let merged = reciprocal_rank_fusion(
            Some(&vector_results),
            Some(&fulltext_results),
            0.9, // 90% weight to vector
            0.1, // 10% weight to fulltext
            60.0,
            10,
        );

        // Doc 1 from vector search should rank higher
        assert_eq!(merged[0].0, 1);
    }

    #[test]
    fn test_rrf_top_k() {
        let vector_results = vec![(1, 0.9), (2, 0.8), (3, 0.7)];
        let fulltext_results = vec![(4, 10.0), (5, 8.0), (6, 6.0)];

        let merged = reciprocal_rank_fusion(
            Some(&vector_results),
            Some(&fulltext_results),
            0.5,
            0.5,
            60.0,
            3, // Only top 3
        );

        assert_eq!(merged.len(), 3);
    }

    #[test]
    fn test_weighted_score_fusion() {
        let vector_results = vec![(1, 0.9), (2, 0.8)];
        let fulltext_results = vec![(2, 10.0), (3, 8.0)];

        let merged =
            weighted_score_fusion(Some(&vector_results), Some(&fulltext_results), 0.5, 0.5, 10);

        // Doc 2 appears in both
        assert!(merged.len() >= 2);
        let top_ids: Vec<u64> = merged.iter().map(|(id, _)| *id).collect();
        assert!(top_ids.contains(&2));
    }

    #[test]
    fn test_rrf_empty_results() {
        let merged = reciprocal_rank_fusion(None, None, 0.5, 0.5, 60.0, 10);
        assert!(merged.is_empty());
    }
}
