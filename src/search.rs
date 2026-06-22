use aho_corasick::{AhoCorasick, MatchKind};
use compact_str::CompactString;
use nucleo_matcher::{pattern::{CaseMatching, Normalization, Pattern},Config, Matcher,};
use rayon::prelude::*;
use unicode_normalization::UnicodeNormalization;
use crate::models::{QueryMatch, TextCandidate};
use crate::utils::{apply_buffer, resolve_highlight};

pub struct StructuralSearchEngine {
    queries: Vec<CompactString>,
    normalized_queries: Vec<String>,
    perfect_scores: Vec<f32>,
    aho_corasick: Option<AhoCorasick>,
}

impl StructuralSearchEngine {
    pub fn new(queries: Vec<CompactString>) -> Self {
        let mut normalized_queries = Vec::with_capacity(queries.len());
        let mut perfect_scores = Vec::with_capacity(queries.len());
        
        let mut matcher = Matcher::new(Config::DEFAULT);

        for query in &queries {
            let normalized = Self::normalize_text(query.as_str());
            
            let pattern = Pattern::parse(
                &normalized, 
                CaseMatching::Respect, 
                Normalization::Never
            );
            
            let utf32_normalized = nucleo_matcher::Utf32String::from(normalized.as_str());
            let perfect_score = pattern.score(utf32_normalized.slice(..), &mut matcher)
                .map(|score| score as f32)
                .unwrap_or(1.0); 

            normalized_queries.push(normalized);
            perfect_scores.push(perfect_score);
        }

        let aho_corasick = AhoCorasick::builder()
            .match_kind(MatchKind::LeftmostFirst)
            .build(&normalized_queries)
            .ok();

        Self { queries, normalized_queries, perfect_scores, aho_corasick }
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
                .filter(|ch| !unicode_normalization::char::is_combining_mark(*ch))
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
        let Some(ac) = &self.aho_corasick else { return candidates.iter().collect(); };
        candidates.iter().filter(|cand| ac.is_match(&cand.normalized_text)).collect()
    }

    pub fn process_candidates(&self, candidates: &[&TextCandidate], threshold: f32, buffer_size: usize) -> Vec<QueryMatch> {
        thread_local! {
            static THREAD_MATCHER: std::cell::RefCell<Matcher> = 
                std::cell::RefCell::new(Matcher::new(Config::DEFAULT));
        }

        candidates.par_iter().flat_map(|&candidate| {
            let mut local_results = Vec::new();

            THREAD_MATCHER.with(|matcher_cell| {
                let mut matcher = matcher_cell.borrow_mut();

                for (idx, query) in self.queries.iter().enumerate() {
                    let norm_query = &self.normalized_queries[idx];
                    let perfect_score = self.perfect_scores[idx];

                    if let Some(byte_idx) = candidate.normalized_text.find(norm_query) {
                        let start_idx_norm = candidate.normalized_text[..byte_idx].chars().count();
                        let query_len_norm = norm_query.chars().count();
                        
                        if query_len_norm == 0 { continue; }
                        let end_idx_norm = start_idx_norm + query_len_norm - 1;

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
                        continue;
                    }

                    let pattern = Pattern::parse(
                        norm_query, 
                        CaseMatching::Respect, 
                        Normalization::Never
                    );

                    let utf32_text = nucleo_matcher::Utf32String::from(candidate.normalized_text.as_str());
                    let mut indices = Vec::new();

                    if let Some(raw_score) = pattern.indices(utf32_text.slice(..), &mut matcher, &mut indices) {
                        if indices.is_empty() { continue; }
                        
                        let start_idx_norm = *indices.iter().min().unwrap() as usize;
                        let end_idx_norm = *indices.iter().max().unwrap() as usize;
                        let match_span_norm = end_idx_norm.saturating_sub(start_idx_norm) + 1;
                        let query_len = norm_query.chars().count();

                        if match_span_norm as f32 > (query_len as f32 * 1.5) { continue; }

                        let scaled_score = (raw_score as f32 / perfect_score) * 99.0;
                        let normalized_score = scaled_score.min(99.0);

                        if normalized_score >= threshold {
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
            });
            local_results
        }).collect()
    }
}