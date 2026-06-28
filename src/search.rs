use aho_corasick::{AhoCorasick, MatchKind};
use compact_str::CompactString;
use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Config, Matcher,
};
use rayon::prelude::*;
use unicode_normalization::UnicodeNormalization;
use crate::models::{QueryMatch, TextCandidate};
use crate::utils::{apply_buffer, resolve_highlight};

pub struct StructuralSearchEngine {
    queries: Vec<CompactString>,
    normalized_queries: Vec<String>,
    alpha_queries: Vec<String>,
    query_ascii_masks: Vec<u128>,
    query_unique_ascii_counts: Vec<usize>,
    perfect_scores: Vec<f32>,
    patterns: Vec<Pattern>,
    aho_corasick: Option<AhoCorasick>,
    bypass_ac_filter: bool,
}

impl StructuralSearchEngine {
    pub fn new(queries: Vec<CompactString>) -> Self {
        let mut normalized_queries = Vec::with_capacity(queries.len());
        let mut alpha_queries = Vec::with_capacity(queries.len());
        let mut query_ascii_masks = Vec::with_capacity(queries.len());
        let mut query_unique_ascii_counts = Vec::with_capacity(queries.len());
        let mut perfect_scores = Vec::with_capacity(queries.len());
        let mut patterns = Vec::with_capacity(queries.len());
        
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut bypass_ac_filter = false;
        let mut sub_words = Vec::new();

        for query in &queries {
            let normalized = Self::normalize_text(query.as_str());
            
            // Build the pure alphanumeric representation of the query to handle layout noise
            let alpha: String = normalized
               .chars()
               .filter(|c| c.is_alphanumeric())
               .collect();
            
            // Build the character-frequency ASCII bitmask
            let mut ascii_mask: u128 = 0;
            let mut unique_chars = std::collections::HashSet::new();
            for c in normalized.chars() {
                if c.is_ascii() {
                    let val = c as u32;
                    if val < 128 {
                        ascii_mask |= 1 << val;
                        unique_chars.insert(c);
                    }
                }
            }
            let unique_ascii_count = unique_chars.len();

            // Pre-compile the Nucleo Pattern to eliminate runtime allocations
            let pattern = Pattern::parse(
                &normalized, 
                CaseMatching::Respect, 
                Normalization::Never
            );
            
            let utf32_normalized = nucleo_matcher::Utf32String::from(normalized.as_str());
            let raw_perfect = pattern.score(utf32_normalized.slice(..), &mut matcher)
               .map(|score| score as f32)
               .unwrap_or(1.0); 
            
            let perfect_score = if raw_perfect <= 0.0 { 1.0 } else { raw_perfect };

            // Extract candidate tokens of length >= 4 for sub-word Aho-Corasick matching
            let mut words_ge_4 = 0;
            for word in normalized.split(|c: char|!c.is_alphanumeric()) {
                if word.chars().count() >= 4 {
                    sub_words.push(word.to_string());
                    words_ge_4 += 1;
                }
            }
            
            if words_ge_4 == 0 {
                bypass_ac_filter = true;
            }

            normalized_queries.push(normalized);
            alpha_queries.push(alpha);
            query_ascii_masks.push(ascii_mask);
            query_unique_ascii_counts.push(unique_ascii_count);
            perfect_scores.push(perfect_score);
            patterns.push(pattern);
        }

        // Deduplicate the sub-word vocabulary to minimize state transitions
        sub_words.sort();
        sub_words.dedup();

        let aho_corasick = if!sub_words.is_empty() {
            AhoCorasick::builder()
               .match_kind(MatchKind::LeftmostFirst)
               .build(&sub_words)
               .ok()
        } else {
            None
        };

        Self {
            queries,
            normalized_queries,
            alpha_queries,
            query_ascii_masks,
            query_unique_ascii_counts,
            perfect_scores,
            patterns,
            aho_corasick,
            bypass_ac_filter,
        }
    }

    pub fn normalize_text(input: &str) -> String {
        Self::normalize_text_with_mapping(input).0
    }

    pub fn normalize_text_with_mapping(input: &str) -> (String, Vec<usize>) {
        let mut normalized = String::with_capacity(input.len());
        let mut mapping = Vec::with_capacity(input.len());

        for (raw_idx, c) in input.chars().enumerate() {
            let mut char_buf = [0; 4];
            let char_str = c.encode_utf8(&mut char_buf);

            let norm_iter = char_str
               .nfd()
               .filter(|ch|!unicode_normalization::char::is_combining_mark(*ch))
               .nfkc()
               .flat_map(|ch| ch.to_lowercase());

            for nc in norm_iter {
                normalized.push(nc);
                mapping.push(raw_idx);
            }
        }
        (normalized, mapping)
    }

    pub fn filter_candidates<'a>(&self, candidates: &'a [TextCandidate]) -> Vec<&'a TextCandidate> {
        let Some(ac) = &self.aho_corasick else {
            return candidates.iter().collect();
        };
        
        if self.bypass_ac_filter {
            return candidates.iter().collect();
        }

        candidates
           .iter()
           .filter(|cand| ac.is_match(&cand.normalized_text))
           .collect()
    }

    pub fn process_candidates(&self, candidates: &[&TextCandidate], threshold: f32, buffer_size: usize) -> Vec<QueryMatch> {
        thread_local! {
            static THREAD_MATCHER: std::cell::RefCell<Matcher> = 
                std::cell::RefCell::new(Matcher::new(Config::DEFAULT));
        }

        candidates.par_iter().flat_map(|&candidate| {
            let mut local_results = Vec::new();

            // Precompute the candidate's bitmask once to avoid redundant string sweeps
            let mut cand_mask: u128 = 0;
            for c in candidate.normalized_text.chars() {
                if c.is_ascii() {
                    let val = c as u32;
                    if val < 128 {
                        cand_mask |= 1 << val;
                    }
                }
            }

            // Construct character index mappings for alphanumeric projection
            let mut cand_alpha = String::with_capacity(candidate.normalized_text.len());
            let mut cand_alpha_mapping = Vec::with_capacity(candidate.normalized_text.len());

            for (char_idx, c) in candidate.normalized_text.chars().enumerate() {
                if c.is_alphanumeric() {
                    cand_alpha.push(c);
                    cand_alpha_mapping.push(char_idx);
                }
            }

            THREAD_MATCHER.with(|matcher_cell| {
                let mut matcher = matcher_cell.borrow_mut();

                for (idx, query) in self.queries.iter().enumerate() {
                    let norm_query = &self.normalized_queries[idx];
                    let perfect_score = self.perfect_scores[idx];
                    let pattern = &self.patterns[idx];

                    // --- LAYER 2: Noise-Agnostic Alphanumeric Perfect Matcher (Score = 100.0) ---
                    let alpha_query = &self.alpha_queries[idx];
                    if!alpha_query.is_empty() {
                        if let Some(byte_offset) = cand_alpha.find(alpha_query) {
                            let start_char_alpha = cand_alpha[..byte_offset].chars().count();
                            let len_char_alpha = alpha_query.chars().count();
                            
                            if len_char_alpha > 0 {
                                let end_char_alpha = start_char_alpha + len_char_alpha - 1;

                                if start_char_alpha < cand_alpha_mapping.len() && end_char_alpha < cand_alpha_mapping.len() {
                                    let start_idx_norm = cand_alpha_mapping[start_char_alpha];
                                    let end_idx_norm = cand_alpha_mapping[end_char_alpha];

                                    if start_idx_norm < candidate.mapping.len() && end_idx_norm < candidate.mapping.len() {
                                        let start_idx_raw = candidate.mapping[start_idx_norm];
                                        let end_idx_raw = candidate.mapping[end_idx_norm];

                                        let raw_indices = [start_idx_raw as u32, end_idx_raw as u32];
                                        let (prefix_raw, match_text, suffix_raw) = resolve_highlight(&candidate.text, &raw_indices);

                                        let prefix = apply_buffer(&prefix_raw, buffer_size, true);
                                        let suffix = apply_buffer(&suffix_raw, buffer_size, false);

                                        local_results.push(QueryMatch {
                                            query: query.clone(),
                                            matches: true,
                                            location: candidate.location.clone(),
                                            similarity_score: 100.0,
                                            prefix: CompactString::from(prefix.trim_start()),
                                            match_text,
                                            suffix: CompactString::from(suffix.trim_end()),
                                        });
                                        continue; // Perfect match found; bypass subsequence alignment
                                    }
                                }
                            }
                        }
                    }

                    // --- LAYER 1: Dynamic Pre-Filter Bypass ---
                    // If threshold is below 70.0, exact pre-filtering is disabled to preserve fuzzy recall
                    if threshold >= 70.0 &&!self.bypass_ac_filter {
                        if let Some(ac) = &self.aho_corasick {
                            if!ac.is_match(&candidate.normalized_text) {
                                continue;
                            }
                        }
                    }

                    // --- LAYER 3: Mathematical Length Pruning Heuristic ---
                    let query_len = norm_query.chars().count();
                    if query_len == 0 { continue; }

                    let min_len_norm = ((query_len as f32) * (threshold / 100.0) * 0.7) as usize;
                    if candidate.normalized_text.chars().count() < min_len_norm {
                        continue;
                    }

                    // --- LAYER 4: Bitmask Character-Frequency Filter ---
                    let q_mask = self.query_ascii_masks[idx];
                    let q_unique_cnt = self.query_unique_ascii_counts[idx];
                    let min_unique_matches = ((q_unique_cnt as f32) * (threshold / 100.0) * 0.75) as usize;

                    if min_unique_matches > 0 {
                        let common_bits = (q_mask & cand_mask).count_ones() as usize;
                        if common_bits < min_unique_matches {
                            continue; // Discard unviable candidate
                        }
                    }

                    // --- LAYER 5: Parallelized Subsequence Alignment ---
                    let utf32_text = nucleo_matcher::Utf32String::from(candidate.normalized_text.as_str());
                    let mut indices = Vec::new();

                    if let Some(raw_score) = pattern.indices(utf32_text.slice(..), &mut matcher, &mut indices) {
                        if indices.is_empty() { continue; }
                        
                        let start_idx_norm = *indices.iter().min().unwrap() as usize;
                        let end_idx_norm = *indices.iter().max().unwrap() as usize;
                        let match_span_norm = end_idx_norm.saturating_sub(start_idx_norm) + 1;

                        // Enforce dynamic match span limit based on user-defined threshold
                        let max_span_factor = 1.0 + ((100.0 - threshold) / 100.0);
                        let max_allowed_span = ((query_len as f32) * max_span_factor).ceil() as usize;

                        if match_span_norm > max_allowed_span { continue; }

                        let scaled_score = (raw_score as f32 / perfect_score) * 99.0;
                        let normalized_score = scaled_score.min(99.0);

                        if normalized_score >= threshold {
                            if start_idx_norm < candidate.mapping.len() && end_idx_norm < candidate.mapping.len() {
                                let start_idx_raw = candidate.mapping[start_idx_norm];
                                let end_idx_raw = candidate.mapping[end_idx_norm];

                                let raw_indices = [start_idx_raw as u32, end_idx_raw as u32];
                                let (prefix_raw, match_text, suffix_raw) = resolve_highlight(&candidate.text, &raw_indices);

                                let prefix = apply_buffer(&prefix_raw, buffer_size, true);
                                let suffix = apply_buffer(&suffix_raw, buffer_size, false);

                                local_results.push(QueryMatch {
                                    query: query.clone(),
                                    matches: true,
                                    location: candidate.location.clone(),
                                    similarity_score: normalized_score,
                                    prefix: CompactString::from(prefix.trim_start()),
                                    match_text,
                                    suffix: CompactString::from(suffix.trim_end()),
                                });
                            }
                        }
                    }
                }
            });
            local_results
        }).collect()
    }
}
