use anyhow::{Context, Result};
use compact_str::CompactString;
use std::{collections::HashMap, path::Path};
use crate::extractor::DocumentExtractor;
use crate::models::{QueryMatch, SpatialIndex, BBox};
use crate::search::StructuralSearchEngine;
use crate::utils::{apply_buffer, resolve_spatial_highlights};

#[derive(Clone)]
pub struct DisplayRow {
    pub query: String,
    pub score: f32,
    pub prefix: String,
    pub match_text: String,
    pub suffix: String,
    pub page_number: usize,           
    pub target_highlights: Vec<BBox>, 
}

impl DisplayRow {
    pub fn full_text(&self) -> String {
        if self.score > 0.0 {
            format!("{}{{{}}}{}", self.prefix, self.match_text, self.suffix)
        } else {
            self.prefix.clone()
        }
    }
}

pub struct TraceTextApp;

impl TraceTextApp {
    pub fn run_search(
        file_path: &Path,
        queries: Vec<CompactString>,
        threshold: f32,
        buffer_size: usize,
        display_limit: usize,
    ) -> Result<Vec<DisplayRow>> {
        let extractor = DocumentExtractor;
        let ext = file_path.extension().and_then(|e| e.to_str())
            .context("Input file lacks a valid extension")?.to_lowercase();
            
        // Phase 1 integration: Unifying the pipeline to output candidates and the SpatialIndex
        let (candidates, spatial_index) = match ext.as_str() {
            "pdf" | "docx" => extractor.extract_unified_pipeline(file_path)?,
            _ => anyhow::bail!("Unsupported file format: {}", ext),
        };

        let engine = StructuralSearchEngine::new(candidates);
        let all_matches = engine.search(&queries);
        
        let successful_matches: Vec<QueryMatch> = all_matches
            .into_iter()
            .filter(|m| m.similarity_score >= threshold)
            .collect();

        // Pass down the SpatialIndex to resolve physical coordinates
        Ok(Self::aggregate_results(
            &queries, 
            &successful_matches, 
            &spatial_index, 
            buffer_size, 
            display_limit
        ))
    }

    fn aggregate_results(
        all_queries: &[CompactString], 
        matches: &[QueryMatch], 
        spatial_index: &SpatialIndex,
        buffer_size: usize, 
        display_limit: usize
    ) -> Vec<DisplayRow> {
        let mut match_map: HashMap<&CompactString, Vec<&QueryMatch>> = HashMap::new();
        for m in matches { match_map.entry(&m.query).or_default().push(m); }

        let mut rows = Vec::new();
        for query in all_queries {
            if let Some(query_matches) = match_map.get(query) {
                for m in query_matches {
                    // Resolve the physical layout metrics using the spatial index
                    let (page_number, target_highlights) = resolve_spatial_highlights(
                        &m.matched_indices, 
                        spatial_index
                    );

                    let prefix_clean = m.prefix.replace('\n', " ");
                    let match_clean = m.match_text.replace('\n', " ");
                    let suffix_clean = m.suffix.replace('\n', " ");

                    let mut final_prefix = apply_buffer(&prefix_clean, buffer_size, true).to_string();
                    let mut final_suffix = apply_buffer(&suffix_clean, buffer_size, false).to_string();

                    let match_len = match_clean.chars().count();
                    let total_len = final_prefix.chars().count() + match_len + final_suffix.chars().count();
                    
                    if total_len > display_limit {
                        let available = display_limit.saturating_sub(match_len);
                        let half = available / 2;
                        
                        final_prefix = apply_buffer(&prefix_clean, half, true).to_string();
                        final_suffix = apply_buffer(&suffix_clean, half, false).to_string();
                    }

                    rows.push(DisplayRow {
                        query: query.to_string(),
                        score: m.similarity_score,
                        prefix: final_prefix,
                        match_text: match_clean,
                        suffix: final_suffix,
                        page_number,
                        target_highlights,
                    });
                }
            } else {
                rows.push(DisplayRow {
                    query: query.to_string(),
                    score: 0.0,
                    prefix: "N/A".to_string(),
                    match_text: "".to_string(),
                    suffix: "".to_string(),
                    page_number: 0,
                    target_highlights: vec![],
                });
            }
        }
        rows
    }
}
