use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

pub const DEFAULT_HYBRID_BACKEND: &str = "local_sparse_hash";
pub const DEFAULT_HYBRID_DIMS: usize = 64;
pub const DEFAULT_HYBRID_FUSION_K: usize = 60;

pub fn embed_sparse_hash(text: &str, dims: usize) -> Vec<f32> {
    let mut vector = vec![0.0_f32; dims];
    if dims == 0 {
        return vector;
    }

    for token in tokenize(text) {
        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        let digest = hasher.finalize();

        let mut index_bytes = [0_u8; 8];
        index_bytes.copy_from_slice(&digest[..8]);
        let index = usize::from_le_bytes(index_bytes) % dims;
        let sign = if digest[8] & 1 == 0 { 1.0 } else { -1.0 };
        vector[index] += sign;
    }

    vector
}

pub fn cosine_similarity(left: &[f32], right: &[f32]) -> f64 {
    let mut dot = 0.0_f64;
    let mut left_norm = 0.0_f64;
    let mut right_norm = 0.0_f64;

    for (left_value, right_value) in left.iter().zip(right.iter()) {
        let left_value = *left_value as f64;
        let right_value = *right_value as f64;
        dot += left_value * right_value;
        left_norm += left_value * left_value;
        right_norm += right_value * right_value;
    }

    if left_norm == 0.0 || right_norm == 0.0 {
        return 0.0;
    }

    dot / left_norm.sqrt() / right_norm.sqrt()
}

pub fn rank_sparse_hash(query: &str, documents: &[(String, String)], dims: usize) -> Vec<String> {
    let query_embedding = embed_sparse_hash(query, dims);
    let documents = documents
        .iter()
        .map(|(id, text)| (id.clone(), embed_sparse_hash(text, dims)))
        .collect::<Vec<_>>();
    rank_sparse_hash_with_embeddings(&query_embedding, &documents)
}

pub fn rank_sparse_hash_with_embeddings(
    query_embedding: &[f32],
    documents: &[(String, Vec<f32>)],
) -> Vec<String> {
    let mut scored = documents
        .iter()
        .map(|(id, embedding)| (id.clone(), cosine_similarity(query_embedding, embedding)))
        .collect::<Vec<_>>();

    scored.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });

    scored.into_iter().map(|(id, _)| id).collect()
}

pub fn reciprocal_rank_fusion(rankings: &[Vec<String>], fusion_k: usize) -> Vec<String> {
    let mut scores = BTreeMap::<String, f64>::new();
    let fusion_k = fusion_k as f64;

    for ranking in rankings {
        let mut seen = BTreeSet::new();
        for (rank, candidate) in ranking.iter().enumerate() {
            if !seen.insert(candidate.clone()) {
                continue;
            }
            let weight = 1.0 / (fusion_k + rank as f64 + 1.0);
            *scores.entry(candidate.clone()).or_insert(0.0) += weight;
        }
    }

    let mut ranked = scores.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    ranked.into_iter().map(|(id, _)| id).collect()
}

pub fn estimated_storage_bytes(record_count: usize, dims: usize) -> usize {
    record_count
        .saturating_mul(dims)
        .saturating_mul(std::mem::size_of::<f32>())
}

fn tokenize(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|ch: char| !ch.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedding_is_deterministic() {
        let left = embed_sparse_hash("Hybrid retrieval for long-history recall", 64);
        let right = embed_sparse_hash("Hybrid retrieval for long-history recall", 64);
        assert_eq!(left, right);
    }

    #[test]
    fn reciprocal_rank_fusion_can_lift_vector_candidate() {
        let fused = reciprocal_rank_fusion(
            &[
                vec![
                    "lexical_only".to_string(),
                    "shared".to_string(),
                    "vector_only".to_string(),
                ],
                vec![
                    "vector_only".to_string(),
                    "other".to_string(),
                    "shared".to_string(),
                    "lexical_only".to_string(),
                ],
            ],
            DEFAULT_HYBRID_FUSION_K,
        );
        assert_eq!(fused.first().map(String::as_str), Some("vector_only"));
        assert!(
            fused
                .iter()
                .position(|id| id == "vector_only")
                .expect("vector rank")
                < fused
                    .iter()
                    .position(|id| id == "lexical_only")
                    .expect("lexical rank")
        );
    }
}
