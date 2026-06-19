#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::{Context, Result};
use std::{path::{Path, PathBuf}, ops::ControlFlow, collections::HashMap};
use unpdf::{PdfParser, PageStreamOptions, ParseEvent};
use undoc::docx::DocxParser;
use unicode_normalization::UnicodeNormalization;
use aho_corasick::{AhoCorasick, MatchKind};
use compact_str::CompactString;
use nucleo_matcher::{Config, Matcher, pattern::{Pattern, CaseMatching, Normalization}};
use rayon::prelude::*;
use eframe::egui;
use egui_extras::{TableBuilder, Column};
use rfd::FileDialog;
use rust_xlsxwriter::Workbook;

// --- Existing Core Logic ---

#[derive(Debug, Clone, serde::Serialize)]
pub struct QueryMatch {
    pub query: CompactString,
    pub matches: bool,
    pub location: StructuralLocation,
    pub similarity_score: f32,
    pub raw_matched_text: CompactString,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StructuralLocation {
    Pdf { 
        page_number: u32, 
        block_index: usize,
    },
    Docx { 
        global_paragraph_index: usize, 
        heading_context: String, 
    },
}

#[derive(Debug, Clone)]
pub struct TextCandidate {
    pub text: String,
    pub location: StructuralLocation,
}

pub struct DocumentExtractor;

impl DocumentExtractor {
    pub fn extract_pdf_stream<P: AsRef<Path>>(&self, path: P) -> Result<Vec<TextCandidate>> {
        let parser = PdfParser::open(path).context("Failed to initialize unpdf parser")?;
        let candidates_accumulator = std::sync::Mutex::new(Vec::new());

        parser.for_each_page(PageStreamOptions::default(), |event| {
            if let ParseEvent::PageParsed(page) = event {
                let mut candidates = candidates_accumulator.lock().unwrap();
                
                // Added .iter().enumerate() to capture the element index
                for (elem_idx, element) in page.elements.iter().enumerate() {
                    let mut page_text = String::new();
                    element.append_plain_text(&mut page_text);
                    
                    if !page_text.trim().is_empty() {
                        candidates.push(TextCandidate {
                            text: page_text,
                            location: StructuralLocation::Pdf { 
                                page_number: page.number,
                                block_index: elem_idx,
                            },
                        });
                    }
                }
            }
            ControlFlow::Continue(())
        }).context("PDF streaming iteration failed")?;

        Ok(candidates_accumulator.into_inner().unwrap())
    }

    pub fn extract_docx<P: AsRef<Path>>(&self, path: P) -> Result<Vec<TextCandidate>> {
        let mut parser = DocxParser::open(path).context("Failed to initialize OOXML DOCX parser")?;
        let doc = parser.parse().context("Failed to parse internal document structures")?;
        let mut candidates = Vec::new();

        let mut current_heading = String::from("Start of Document");
        let mut global_para_count = 0; // Initialize global counter

        for section in doc.sections.iter() {
            for block in section.content.iter() {
                let text = match block {
                    undoc::Block::Paragraph(para) => para.plain_text(),
                    undoc::Block::Table(table) => table.plain_text(),
                    _ => String::new(),
                };
                
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    continue;
                }

                // Increment for every valid block of text
                global_para_count += 1;

                let char_count = trimmed.chars().count();
                if char_count > 2 && char_count < 85 && !trimmed.ends_with('.') && !trimmed.ends_with(':') {
                    current_heading = trimmed.chars().take(50).collect::<String>();
                    if char_count > 50 {
                        current_heading.push_str("...");
                    }
                }

                candidates.push(TextCandidate {
                    text: text.clone(),
                    location: StructuralLocation::Docx { 
                        global_paragraph_index: global_para_count, // Use global count
                        heading_context: current_heading.clone(),
                    },
                });
            }
        }
        Ok(candidates)
    }
}

pub struct StructuralSearchEngine {
    queries: Vec<CompactString>,
    aho_corasick: Option<AhoCorasick>,
}

impl StructuralSearchEngine {
    pub fn new(queries: Vec<CompactString>) -> Self {
        let patterns: Vec<String> = queries.iter().map(|q| Self::normalize_text(q)).collect();
        let aho_corasick = AhoCorasick::builder()
            .match_kind(MatchKind::LeftmostFirst)
            .build(patterns)
            .ok();
        Self { queries, aho_corasick }
    }

    pub fn filter_candidates<'a>(&self, candidates: &'a [TextCandidate]) -> Vec<&'a TextCandidate> {
        let Some(ac) = &self.aho_corasick else { return candidates.iter().collect(); };
        candidates.iter().filter(|cand| ac.is_match(&Self::normalize_text(&cand.text))).collect()
    }

    pub fn process_candidates(&self, candidates: &[&TextCandidate], threshold: f32) -> Vec<QueryMatch> {
        let normalized_candidates: Vec<(&TextCandidate, String)> = candidates
            .par_iter()
            .map(|&cand| (cand, Self::normalize_text(&cand.text)))
            .collect();

        self.queries.par_iter().flat_map(|query| {
            let mut matcher = Matcher::new(Config::DEFAULT);
            let mut results = Vec::new();
            let pattern = Pattern::parse(query.as_str(), CaseMatching::Ignore, Normalization::Smart);
            let utf32_query = nucleo_matcher::Utf32String::from(query.as_str());
            let perfect_score = pattern.score(utf32_query.slice(..), &mut matcher).unwrap_or(1) as f32;
            let mut indices = Vec::new();

            for (candidate, normalized_text) in &normalized_candidates {
                indices.clear();
                let utf32_text = nucleo_matcher::Utf32String::from(normalized_text.as_str());
                
                if let Some(score) = pattern.indices(utf32_text.slice(..), &mut matcher, &mut indices) {
                    let mut normalized_score = (score as f32 / perfect_score) * 100.0;
                    normalized_score = normalized_score.min(100.0);

                    if normalized_score >= threshold && !indices.is_empty() {
                        let start_idx = *indices.first().unwrap() as usize;
                        let end_idx = *indices.last().unwrap() as usize;
                        let match_span = end_idx.saturating_sub(start_idx) + 1;
                        
                        let query_len = query.chars().count();
                        if match_span > (query_len as f32 * 2.5) as usize { continue; }

                        let buffer = 5;
                        let snippet_start = start_idx.saturating_sub(buffer);
                        let snippet_len = match_span + (buffer * 2);

                        let mut snippet: String = candidate.text.chars().skip(snippet_start).take(snippet_len).collect();
                        if snippet_start > 0 {
                            snippet.insert_str(0, "...");
                        }
                        if snippet_start + snippet_len < candidate.text.chars().count() {
                            snippet.push_str("...");
                        }
                        results.push(QueryMatch {
                            query: query.clone(),
                            matches: true,
                            location: candidate.location.clone(),
                            similarity_score: normalized_score,
                            raw_matched_text: CompactString::from(snippet.trim()),
                        });
                    }
                }
            }
            results
        }).collect()
    }

    pub fn normalize_text(input: &str) -> String {
        input.nfd().filter(|c| !unicode_normalization::char::is_combining_mark(*c))
            .nfkc().flat_map(|c| c.to_lowercase()).collect()
    }
}

#[derive(Clone)]
pub struct DisplayRow {
    pub query: String,
    pub matched: String,
    pub raw_text: String,
    pub location: String,
    pub score: f32,
}

pub struct TraceTextApp;

impl TraceTextApp {
    /// Returns the aggregated rows instead of printing them
    pub fn run_search(
        file_path: &Path,
        queries: Vec<CompactString>,
        threshold: f32,
    ) -> Result<Vec<DisplayRow>> {
        let extractor = DocumentExtractor;
        let ext = file_path.extension().and_then(|e| e.to_str())
            .context("Input file lacks a valid extension")?.to_lowercase();
            
        let candidates = match ext.as_str() {
            "pdf" => extractor.extract_pdf_stream(file_path)?,
            "docx" => extractor.extract_docx(file_path)?,
            _ => anyhow::bail!("Unsupported file format: {}", ext),
        };

        let engine = StructuralSearchEngine::new(queries.clone());
        let candidate_refs: Vec<&TextCandidate> = candidates.iter().collect();
        // Option to uncomment pre-filtering: let filtered_candidates = engine.filter_candidates(&candidates);
        let successful_matches = engine.process_candidates(&candidate_refs, threshold);

        Ok(Self::aggregate_results(&queries, &successful_matches))
    }

    fn aggregate_results(all_queries: &[CompactString], matches: &[QueryMatch]) -> Vec<DisplayRow> {
        let mut match_map: HashMap<&CompactString, Vec<&QueryMatch>> = HashMap::new();
        for m in matches { match_map.entry(&m.query).or_default().push(m); }

        let mut rows = Vec::new();
        for query in all_queries {
            if let Some(query_matches) = match_map.get(query) {
                for m in query_matches {
                    let location_str = match &m.location {
                        StructuralLocation::Pdf { page_number, block_index } => {
                            format!("Page {} (Block {})", page_number, block_index)
                        },
                        StructuralLocation::Docx { global_paragraph_index, heading_context } => {
                            // Yields: Heading: "Validation Protocol" (Paragraph 142)
                            format!("Heading: \"{}\" (Paragraph {})", heading_context, global_paragraph_index)
                        },
                    };
                    
                    let sanitized_text = m.raw_matched_text.replace('\n', " ");
                    let display_limit = 60; 
                    let raw_text = if sanitized_text.chars().count() > display_limit {
                        format!("{}...", sanitized_text.chars().take(display_limit).collect::<String>())
                    } else {
                        sanitized_text
                    };

                    rows.push(DisplayRow {
                        query: query.to_string(),
                        matched: "Yes".to_string(),
                        raw_text,
                        location: location_str,
                        score: m.similarity_score,
                    });
                }
            } else {
                rows.push(DisplayRow {
                    query: query.to_string(),
                    matched: "No".to_string(),
                    raw_text: "N/A".to_string(),
                    location: "N/A".to_string(),
                    score: 0.0,
                });
            }
        }
        rows
    }
}

// --- GUI Implementation ---

struct TraceTextGui {
    file_path: Option<PathBuf>,
    queries_text: String,
    threshold: f32,
    results: Vec<DisplayRow>,
    status_message: String,
}

impl Default for TraceTextGui {
    fn default() -> Self {
        Self {
            file_path: None,
            //queries_text: "Verification and Validation of Results\nProcess Automation\nScalability and Performance\nABCDE".to_string(),
            queries_text: "".to_string(),
            threshold: 90.0,
            results: Vec::new(),
            status_message: "Ready. Select a file to begin.".into(),
        }
    }
}

impl eframe::App for TraceTextGui {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // 2. Use `show_inside` and pass the `ui` instead of `ctx`
        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.heading("TraceText - Structural Document Search");
            ui.add_space(10.0);

            // 1. File Picker
            ui.horizontal(|ui| {
                if ui.button("📂 Select PDF/Word Document").clicked() {
                    if let Some(path) = FileDialog::new()
                        .add_filter("Documents", &["pdf", "docx"])
                        .pick_file() {
                        self.file_path = Some(path);
                        self.status_message = "File loaded.".into();
                    }
                }
                if let Some(path) = &self.file_path {
                    ui.label(path.display().to_string());
                } else {
                    ui.label("No file selected.");
                }
            });
            ui.add_space(10.0);

            // 2. Input Queries
            ui.label("Search Queries (One per line):");
            ui.add(egui::TextEdit::multiline(&mut self.queries_text).desired_rows(5).desired_width(f32::INFINITY));
            ui.add_space(10.0);

            // 3. Threshold Slider
            ui.add(egui::Slider::new(&mut self.threshold, 0.0..=100.0).text("Similarity Threshold (%)"));
            ui.add_space(10.0);

            // 4. Run Button & Status
            ui.horizontal(|ui| {
                if ui.button("🚀 Run Search").clicked() {
                    if let Some(path) = &self.file_path {
                        //self.status_message = "Processing...".into();
                        
                        let queries: Vec<CompactString> = self.queries_text
                            .lines()
                            .filter(|l| !l.trim().is_empty())
                            .map(|l| CompactString::from(l.trim()))
                            .collect();

                        match TraceTextApp::run_search(path, queries, self.threshold) {
                            Ok(res) => {
                                self.results = res;
                                self.status_message = format!("Found {} result rows.", self.results.len());
                            }
                            Err(e) => {
                                self.status_message = format!("Error: {}", e);
                            }
                        }
                    } else {
                        self.status_message = "Error: Please select a document first.".into();
                    }
                }
                ui.label(egui::RichText::new(&self.status_message).color(egui::Color32::DARK_GRAY));
            });
            ui.add_space(10.0);

            ui.separator();
            ui.add_space(10.0);

            // 5. Output Actions (Copy & Export)
            if !self.results.is_empty() {
                ui.horizontal(|ui| {
                    if ui.button("📋 Copy Table").clicked() {
                        let tsv = format_clipboard_tsv(&self.results);
                        
                        // Use the new clipboard API here
                        ui.ctx().copy_text(tsv); 
                        
                        self.status_message = "Table copied to clipboard!".into();
                    }

                    if ui.button("📊 Export to Excel").clicked() {
                        if let Some(save_path) = FileDialog::new()
                            .add_filter("Excel Workbook", &["xlsx"])
                            .set_file_name("TraceText_Results.xlsx")
                            .save_file() {
                            match export_to_excel(&self.results, &save_path) {
                                Ok(_) => self.status_message = "Exported successfully!".into(),
                                Err(e) => self.status_message = format!("Export failed: {}", e),
                            }
                        }
                    }
                });
                ui.add_space(10.0);

                // 6. Data Table Rendering
                TableBuilder::new(ui)
                    .striped(true)
                    .resizable(true)
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::initial(200.0).clip(true))
                    .column(Column::initial(70.0).clip(true))
                    .column(Column::remainder().clip(true))
                    .column(Column::initial(150.0).clip(true))
                    .column(Column::initial(60.0).clip(true))
                    .header(20.0, |mut header| {
                        header.col(|ui| { ui.strong("Search Query"); });
                        header.col(|ui| { ui.strong("Matched"); });
                        header.col(|ui| { ui.strong("Raw Matched Text"); });
                        header.col(|ui| { ui.strong("Location"); });
                        header.col(|ui| { ui.strong("Score"); });
                    })
                    .body(|mut body| {
                        for row in &self.results {
                            body.row(25.0, |mut ui_row| {
                                ui_row.col(|ui| { ui.label(&row.query); });
                                ui_row.col(|ui| { ui.label(&row.matched); });
                                ui_row.col(|ui| { ui.label(&row.raw_text); });
                                ui_row.col(|ui| { ui.label(&row.location); });
                                ui_row.col(|ui| { ui.label(format!("{:.2}", row.score)); });
                            });
                        }
                    });
            }
        });
    }
}

// --- Helper Functions ---

fn format_clipboard_tsv(results: &[DisplayRow]) -> String {
    let mut tsv = String::from("Search Query\tMatched\tRaw Matched Text\tStructural Location\tScore\n");
    for r in results {
        tsv.push_str(&format!("{}\t{}\t{}\t{}\t{:.2}\n", r.query, r.matched, r.raw_text, r.location, r.score));
    }
    tsv
}

fn export_to_excel(results: &[DisplayRow], path: &Path) -> Result<()> {
    let mut workbook = Workbook::new();
    let worksheet = workbook.add_worksheet();

    worksheet.write_string(0, 0, "Search Query")?;
    worksheet.write_string(0, 1, "Matched")?;
    worksheet.write_string(0, 2, "Raw Matched Text")?;
    worksheet.write_string(0, 3, "Structural Location")?;
    worksheet.write_string(0, 4, "Score")?;

    for (row_idx, row) in results.iter().enumerate() {
        let r = (row_idx + 1) as u32;
        worksheet.write_string(r, 0, &row.query)?;
        worksheet.write_string(r, 1, &row.matched)?;
        worksheet.write_string(r, 2, &row.raw_text)?;
        worksheet.write_string(r, 3, &row.location)?;
        worksheet.write_number(r, 4, row.score as f64)?;
    }

    workbook.save(path)?;
    Ok(())
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 600.0])
            .with_min_inner_size([600.0, 400.0]),
        ..Default::default()
    };
    
    eframe::run_native(
        "TraceText",
        options,
        Box::new(|_cc| Ok(Box::<TraceTextGui>::default())),
    )
}