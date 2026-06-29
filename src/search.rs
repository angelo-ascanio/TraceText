use crate::models::{QueryMatch, TextCandidate}; //StructuralLocation};
use crate::utils::resolve_highlight;
use compact_str::CompactString;
use rayon::prelude::*;
use unicode_normalization::UnicodeNormalization;
use std::collections::HashSet;

/// High-performance search engine executing multi-layered fuzzy matching
/// and token-agnostic alignment.
pub struct StructuralSearchEngine {
    candidates: Vec<TextCandidate>,
}

impl StructuralSearchEngine {
    /// Creates a new search engine instance with a corpus of text candidates.
    pub fn new(candidates: Vec<TextCandidate>) -> Self {
        Self { candidates }
    }

    /// Normalizes raw input text, removing diacritics, layout noise, and punctuation,
    /// while building a mapping array linking normalized character indices back to original character offsets.
    pub fn normalize_text_with_mapping(text: &str) -> (String, Vec<usize>) {
        let mut normalized_text = String::new();
        let mut mapping = Vec::new();

        let mut char_idx = 0;
        let mut last_was_space = true;

        for c in text.chars() {
            let is_whitespace = c.is_whitespace() || c == '\n' || c == '\r' || c == '\t';
            let is_punctuation = c.is_ascii_punctuation() || "“”‘’*_-•".contains(c);

            if is_whitespace {
                if!last_was_space {
                    normalized_text.push(' ');
                    mapping.push(char_idx);
                    last_was_space = true;
                }
            } else if !is_punctuation {
                // Canonical Decomposition (NFD) to separate base characters from combining diacritics
                let decomposed: String = c.to_string().nfd().collect();
                for dc in decomposed.chars() {
                    // Filter out combining diacritical marks (U+0300 through U+036F)
                    if!('\u{0300}'..='\u{036F}').contains(&dc) {
                        for lc in dc.to_lowercase() {
                            normalized_text.push(lc);
                            mapping.push(char_idx);
                        }
                        last_was_space = false;
                    }
                }
            }
            char_idx += 1;
        }

        // Clean trailing whitespaces if present
        if normalized_text.ends_with(' ') {
            normalized_text.pop();
            mapping.pop();
        }

        (normalized_text, mapping)
    }

    /// Searches the candidate corpus in parallel against a slice of query strings.
    /// Returns matching records containing location information and context segments.
    pub fn search(&self, queries: &[CompactString]) -> Vec<QueryMatch> {
        queries
           .par_iter()
           .flat_map(|query| self.search_single_query(query))
           .collect()
    }

    /// Executes search matching for a single query across all candidates.
    fn search_single_query(&self, query: &CompactString) -> Vec<QueryMatch> {
        let (query_norm, _) = Self::normalize_text_with_mapping(query.as_str());
        if query_norm.is_empty() {
            return Vec::new();
        }

        self.candidates
            .par_iter()
            .flat_map(|candidate| {
                let mut matches = Vec::new();
                let cand_norm = &candidate.normalized_text;
                if cand_norm.is_empty() {
                    return matches;
                }

                let c_chars: Vec<char> = cand_norm.chars().collect();
                let q_chars_count = query_norm.chars().count();
                let mut found_regions: Vec<(usize, usize)> = Vec::new();

                // Helper to prevent fuzzy matches from duplicating Layer 1 matches
                let overlaps = |start: usize, end: usize, regions: &[(usize, usize)]| -> bool {
                    regions.iter().any(|&(s, e)| start <= e && end >= s)
                };

                // LAYER 1 & 2: Extract ALL Exact/Normalized Matches (No Early Exits)
                for (start_byte_idx, _) in cand_norm.match_indices(&query_norm) {
                    let start_char_idx = cand_norm[..start_byte_idx].chars().count();
                    let end_char_idx = start_char_idx + q_chars_count - 1;

                    found_regions.push((start_char_idx, end_char_idx));

                    let mut indices = Vec::new();
                    for i in start_char_idx..=end_char_idx {
                        if i < candidate.mapping.len() {
                            indices.push(candidate.mapping[i] as u32);
                        }
                    }

                    let (prefix, match_text, suffix) = resolve_highlight(&candidate.text, &indices);
                    
                    // FIX: Check if the isolated `match_text` is the literal query 
                    // Using global `.contains()` would falsely score case-insensitive typos as 100 
                    let score = if match_text == query.as_str() { 100.0 } else { 98.0 };

                    matches.push(QueryMatch {
                        query: query.clone(),
                        matches: true,
                        location: candidate.location.clone(),
                        similarity_score: score,
                        prefix,
                        match_text,
                        suffix,
                    });
                }

                // LAYER 3: Blended Fuzzy Matcher (Sliding window to catch typos/acronyms in the remaining file)
                let mut search_start = 0;
                while search_start < c_chars.len() {
                    if let Some((start_norm, end_norm)) = Self::find_best_fuzzy_window_in_range(&query_norm, &c_chars, search_start) {
                        if overlaps(start_norm, end_norm, &found_regions) {
                            search_start = end_norm + 1; // Move past overlap
                            continue;
                        }

                        let window_str: String = c_chars[start_norm..=end_norm].iter().collect();
                        
                        // FIX: Score against the extracted `window_str`, NOT the whole file!
                        let score = Self::compute_fuzzy_score(&query_norm, &window_str);

                        // Slightly lowered threshold to 50.0 to securely catch abbreviations (V&V)
                        if score >= 50.0 {
                            found_regions.push((start_norm, end_norm));
                            
                            let mut indices = Vec::new();
                            for i in start_norm..=end_norm {
                                if i < candidate.mapping.len() {
                                    indices.push(candidate.mapping[i] as u32);
                                }
                            }

                            let (prefix, match_text, suffix) = resolve_highlight(&candidate.text, &indices);

                            matches.push(QueryMatch {
                                query: query.clone(),
                                matches: true,
                                location: candidate.location.clone(),
                                similarity_score: score,
                                prefix,
                                match_text,
                                suffix,
                            });
                        }
                        search_start = end_norm + 1;
                    } else {
                        break;
                    }
                }
                matches
            })
            .collect()
    }

    /// Computes a robust similarity score blending Token Sort, Token Set, and morphological alignment.
    fn compute_fuzzy_score(query_norm: &str, cand_norm: &str) -> f32 {
        let q_tokens: Vec<&str> = query_norm.split_whitespace().collect();
        let c_tokens: Vec<&str> = cand_norm.split_whitespace().collect();

        if q_tokens.is_empty() || c_tokens.is_empty() {
            return 0.0;
        }

        // 1. Token Sort Ratio (reordering-agnostic)
        let mut q_sorted = q_tokens.clone();
        q_sorted.sort_unstable();
        let q_sorted_str = q_sorted.join(" ");

        let mut c_sorted = c_tokens.clone();
        c_sorted.sort_unstable();
        let c_sorted_str = c_sorted.join(" ");

        let token_sort_sim = strsim::normalized_levenshtein(&q_sorted_str, &c_sorted_str) as f32;

        // 2. Token Set Ratio (abbreviation and subset-agnostic)
        let q_set: HashSet<&str> = q_tokens.iter().copied().collect();
        let c_set: HashSet<&str> = c_tokens.iter().copied().collect();

        let intersection: Vec<&str> = q_set.intersection(&c_set).copied().collect();
        let diff_q: Vec<&str> = q_set.difference(&c_set).copied().collect();
        let diff_c: Vec<&str> = c_set.difference(&q_set).copied().collect();

        let mut inter_sorted = intersection.clone();
        inter_sorted.sort_unstable();
        let t0 = inter_sorted.join(" ");

        let mut t1_vec = intersection.clone();
        t1_vec.extend(diff_q);
        t1_vec.sort_unstable();
        let t1 = t1_vec.join(" ");

        let mut t2_vec = intersection.clone();
        t2_vec.extend(diff_c);
        t2_vec.sort_unstable();
        let t2 = t2_vec.join(" ");

        let sim_t0_t1 = strsim::normalized_levenshtein(&t0, &t1) as f32;
        let sim_t0_t2 = strsim::normalized_levenshtein(&t0, &t2) as f32;
        let sim_t1_t2 = strsim::normalized_levenshtein(&t1, &t2) as f32;
        let token_set_sim = sim_t0_t1.max(sim_t0_t2).max(sim_t1_t2);

        // 3. Word-Level Morphological Alignment (translation and typo resilience)
        let mut total_morph_score = 0.0;
        for &qt in &q_tokens {
            let mut max_word_sim = 0.0;
            for &ct in &c_tokens {
                let sim = strsim::jaro_winkler(qt, ct) as f32;
                if sim > max_word_sim {
                    max_word_sim = sim;
                }
            }
            total_morph_score += max_word_sim;
        }
        let morph_alignment_sim = total_morph_score / (q_tokens.len() as f32);

        // 4. Global Structural String Metrics
        let jaro_winkler_sim = strsim::jaro_winkler(query_norm, cand_norm) as f32;
        let normalized_lev_sim = strsim::normalized_levenshtein(query_norm, cand_norm) as f32;
        let sorensen_dice_sim = strsim::sorensen_dice(query_norm, cand_norm) as f32;

        // Blending coefficients: 40% Token Sort, 30% Token Set, 30% Morphological Alignment
        let raw_blend = (token_sort_sim * 0.40) + (token_set_sim * 0.30) + (morph_alignment_sim * 0.30);

        // Combine with global metrics to anchor structural alignment
        let structural_anchor = (jaro_winkler_sim * 0.40) + (normalized_lev_sim * 0.30) + (sorensen_dice_sim * 0.30);
        let final_raw_score = (raw_blend * 0.70) + (structural_anchor * 0.30);

        // Scale to [0.0 - 95.0] to reserve perfect scores for Layer 1 & 2 matches
        final_raw_score * 95.0
    }

    // /// Identifies the optimal sliding window within the candidate text to extract highlighting boundaries.
    // fn find_best_fuzzy_window(query_norm: &str, cand_norm: &str) -> (usize, usize) {
    //     let q_chars: Vec<char> = query_norm.chars().collect();
    //     let c_chars: Vec<char> = cand_norm.chars().collect();

    //     let q_len = q_chars.len();
    //     let c_len = c_chars.len();

    //     if c_len <= q_len {
    //         return (0, c_len.saturating_sub(1));
    //     }

    //     let mut best_score = -1.0;
    //     let mut best_range = (0, c_len.saturating_sub(1));

    //     // Establish boundaries for sliding window sizing
    //     let win_min = (q_len as f32 * 0.7) as usize;
    //     let win_max = (q_len as f32 * 1.4) as usize;

    //     // Perform optimized sliding steps for performance under large strings
    //     let step = if c_len > 150 { (c_len / 40).max(1) } else { 1 };

    //     for start in (0..c_len).step_by(step) {
    //         for win_size in win_min..=win_max {
    //             if start + win_size > c_len {
    //                 break;
    //             }

    //             let window_str: String = c_chars[start..(start + win_size)].iter().collect();
    //             let score = strsim::jaro_winkler(query_norm, &window_str) as f32;

    //             if score > best_score {
    //                 best_score = score;
    //                 best_range = (start, start + win_size - 1);
    //             }
    //         }
    //     }

    //     best_range
    // }

    /// Identifies the optimal sliding window within a localized range to extract highlighting boundaries.
    fn find_best_fuzzy_window_in_range(query_norm: &str, c_chars: &[char], search_start: usize) -> Option<(usize, usize)> {
        let q_len = query_norm.chars().count();
        let c_len = c_chars.len();
        
        if search_start + (q_len as f32 * 0.5) as usize > c_len {
            return None;
        }

        // 1. Fast Hotspot Scan: Stops us from skipping over matches in large files
        let step = if c_len - search_start > 150 { 3 } else { 1 };
        let mut hotspot_start = search_start;
        let mut hotspot_score = -1.0;

        for start in (search_start..c_len).step_by(step) {
            if start + q_len > c_len { break; }
            let window_str: String = c_chars[start..(start + q_len)].iter().collect();
            let score = strsim::jaro_winkler(query_norm, &window_str) as f32;
            
            if score > hotspot_score {
                hotspot_score = score;
                hotspot_start = start;
            }
        }

        if hotspot_score < 0.4 {
            return None; // No meaningful similarity nearby
        }

        // 2. Localized Refinement: Test exact substring lengths around our hotspot
        let win_min = (q_len as f32 * 0.7) as usize;
        let win_max = (q_len as f32 * 1.4) as usize;
        let search_start_refine = hotspot_start.saturating_sub(q_len / 2).max(search_start);
        let search_end_refine = (hotspot_start + q_len / 2).min(c_len);

        let mut best_score = -1.0;
        let mut best_range = (0, 0);

        for start in search_start_refine..=search_end_refine {
            for win_size in win_min..=win_max {
                if start + win_size > c_len { break; }
                let window_str: String = c_chars[start..(start + win_size)].iter().collect();
                let score = strsim::jaro_winkler(query_norm, &window_str) as f32;
                
                if score > best_score {
                    best_score = score;
                    best_range = (start, start + win_size - 1);
                }
            }
        }

        if best_score > 0.0 {
            Some(best_range)
        } else {
            None
        }
    }
}
