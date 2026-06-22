#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::{Context, Result};
use std::{sync::{Arc, RwLock, mpsc::{channel, Sender, Receiver}}, path::{Path, PathBuf}, ops::ControlFlow, collections::HashMap};
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

// --- Phase 1: Foundational Data Structures ---

#[derive(Debug, Clone, serde::Serialize)]
pub struct QueryMatch {
    pub query: CompactString,
    pub matches: bool,
    pub location: StructuralLocation,
    pub similarity_score: f32,
    pub prefix: CompactString,
    pub match_text: CompactString,
    pub suffix: CompactString,
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
    pub normalized_text: String,
    pub mapping: Vec<usize>,
    pub location: StructuralLocation,
}

pub struct DocumentExtractor;

impl DocumentExtractor {
    pub fn extract_pdf_stream<P: AsRef<Path>>(&self, path: P) -> Result<Vec<TextCandidate>> {
        let parser = PdfParser::open(path).context("Failed to initialize unpdf parser")?;
        let candidates_accumulator = std::sync::Mutex::new(Vec::new());

        parser.for_each_page(PageStreamOptions::default(), |event| {
            if let ParseEvent::PageParsed(page) = event {
                let mut page_candidates = Vec::new(); 
                
                for (elem_idx, element) in page.elements.iter().enumerate() {
                    let mut page_text = String::new();
                    element.append_plain_text(&mut page_text);
                    
                    if !page_text.trim().is_empty() {
                        let raw_text = page_text.clone();
                        let (normalized_text, mapping) = StructuralSearchEngine::normalize_text_with_mapping(&raw_text);

                        page_candidates.push(TextCandidate {
                            text: raw_text,
                            normalized_text,
                            mapping,
                            location: StructuralLocation::Pdf { 
                                page_number: page.number,
                                block_index: elem_idx,
                            },
                        });
                    }
                }

                if !page_candidates.is_empty() {
                    candidates_accumulator.lock().unwrap().extend(page_candidates);
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
        let mut global_para_count = 0; 

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

                global_para_count += 1;

                let char_count = trimmed.chars().count();
                if char_count > 2 && char_count < 85 && !trimmed.ends_with('.') && !trimmed.ends_with(':') {
                    current_heading = trimmed.chars().take(50).collect::<String>();
                    if char_count > 50 {
                        current_heading.push_str("...");
                    }
                }

                let raw_text = text.clone();
                let (normalized_text, mapping) = StructuralSearchEngine::normalize_text_with_mapping(&raw_text);

                candidates.push(TextCandidate {
                    text: raw_text,
                    normalized_text,
                    mapping,
                    location: StructuralLocation::Docx { 
                        global_paragraph_index: global_para_count,
                        heading_context: current_heading.clone(),
                    },
                });
            }
        }
        Ok(candidates)
    }
}

// --- Phase 2: Multi-Step Structural Search Engine ---

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

#[derive(Clone)]
pub struct DisplayRow {
    pub query: String,
    pub score: f32,
    pub prefix: String,
    pub match_text: String,
    pub suffix: String,
    pub location: String,
    pub raw_location: Option<StructuralLocation>,
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
            
        let candidates = match ext.as_str() {
            "pdf" => extractor.extract_pdf_stream(file_path)?,
            "docx" => extractor.extract_docx(file_path)?,
            _ => anyhow::bail!("Unsupported file format: {}", ext),
        };

        let engine = StructuralSearchEngine::new(queries.clone());
        
        let filtered_candidates = engine.filter_candidates(&candidates);
        let successful_matches = engine.process_candidates(&filtered_candidates, threshold, buffer_size);

        Ok(Self::aggregate_results(&queries, &successful_matches, display_limit))
    }

    fn aggregate_results(all_queries: &[CompactString], matches: &[QueryMatch], display_limit: usize) -> Vec<DisplayRow> {
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
                            format!("Heading: \"{}\" (Paragraph {})", heading_context, global_paragraph_index)
                        },
                    };
                    
                    let mut prefix_clean = m.prefix.replace('\n', " ");
                    let match_clean = m.match_text.replace('\n', " ");
                    let mut suffix_clean = m.suffix.replace('\n', " ");

                    let match_len = match_clean.chars().count();
                    let total_len = prefix_clean.chars().count() + match_len + suffix_clean.chars().count();
                    
                    if total_len > display_limit {
                        let available = display_limit.saturating_sub(match_len);
                        let half = available / 2;
                        
                        if prefix_clean.chars().count() > half {
                            let skip_amt = prefix_clean.chars().count() - half;
                            prefix_clean = format!("...{}", prefix_clean.chars().skip(skip_amt).collect::<String>());
                        }
                        if suffix_clean.chars().count() > half {
                            suffix_clean = format!("{}...", suffix_clean.chars().take(half).collect::<String>());
                        }
                    }

                    rows.push(DisplayRow {
                        query: query.to_string(),
                        score: m.similarity_score,
                        prefix: prefix_clean,
                        match_text: match_clean,
                        suffix: suffix_clean,
                        location: location_str,
                        raw_location: Some(m.location.clone()),
                    });
                }
            } else {
                rows.push(DisplayRow {
                    query: query.to_string(),
                    score: 0.0,
                    prefix: "N/A".to_string(),
                    match_text: "".to_string(),
                    suffix: "".to_string(),
                    location: "N/A".to_string(),
                    raw_location: None,
                });
            }
        }
        rows
    }
}

// --- GUI Implementation ---

pub struct TraceTextGui {
    file_path: Option<PathBuf>,
    queries_text: String,
    threshold: f32,
    buffer_size: usize,
    display_limit: usize,
    results: Vec<DisplayRow>,
    status_message: String,

    selected_row_index: Option<usize>,

    tx_request: Sender<ParserRequest>,
    rx_response: Receiver<ParserResponse>,
    
    doc_cache: Arc<RwLock<HashMap<PathBuf, CachedDocument>>>,
    
    active_visualization: Option<ParserResponse>,
    pending_scroll_target: Option<StructuralLocation>,

    active_match_text: Option<String>,
}

impl TraceTextGui {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let doc_cache = Arc::new(RwLock::new(HashMap::new()));
        
        let (tx_request, rx_request) = channel::<ParserRequest>();
        let (tx_response, rx_response) = channel::<ParserResponse>();
        
        let doc_cache_clone = Arc::clone(&doc_cache);
        let egui_ctx = cc.egui_ctx.clone();

        std::thread::spawn(move || {
            while let Ok(request) = rx_request.recv() {
                let cache_check = {
                    let r_lock = doc_cache_clone.read().unwrap();
                    r_lock.get(&request.file_path).cloned()
                };

                let doc = match cache_check {
                    Some(cached_doc) => cached_doc,
                    None => {
                        let parsed_doc = parse_document_by_type(&request.file_path);
                        let mut w_lock = doc_cache_clone.write().unwrap();
                        w_lock.insert(request.file_path.clone(), parsed_doc.clone());
                        parsed_doc
                    }
                };

                let response = ParserResponse {
                    file_path: request.file_path,
                    document: doc,
                    target_location: request.target_location,
                };

                let _ = tx_response.send(response);
                egui_ctx.request_repaint();
            }
        });

        Self {
            file_path: None,
            queries_text: "".to_string(),
            threshold: 85.0,
            buffer_size: 100,
            display_limit: 200,
            results: Vec::new(),
            status_message: "Listo. Selecciona un archivo para comenzar.".into(),
            
            selected_row_index: None,
            tx_request,
            rx_response,
            doc_cache,
            active_visualization: None,
            pending_scroll_target: None,
            active_match_text: None,
        }
    }

    fn draw_context_visualizer(&mut self, ui: &mut egui::Ui) {
        if self.active_visualization.is_none() || self.active_match_text.is_none() {
            ui.centered_and_justified(|ui| {
                ui.label(
                    egui::RichText::new("Seleccione una fila de resultados para visualizar el contexto...")
                        .color(egui::Color32::DARK_GRAY)
                        .size(16.0)
                );
            });
            return;
        }

        let visual_data = self.active_visualization.as_ref().unwrap();
        let match_text = self.active_match_text.as_ref().unwrap();
        let pending_scroll = &mut self.pending_scroll_target;

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                match &visual_data.document {
                    CachedDocument::Pdf { pages } => {
                        for (p_idx, paragraphs) in pages.iter().enumerate() {
                            for para in paragraphs {
                                let is_target = match &visual_data.target_location {
                                    StructuralLocation::Pdf { page_number, block_index } => {
                                        *page_number as usize == p_idx + 1 && *block_index == para.original_index
                                    },
                                    _ => false,
                                };
                                
                                Self::render_paragraph(ui, para, is_target, match_text, pending_scroll);
                            }
                        }
                    },
                    CachedDocument::Docx { paragraphs } => {
                        for para in paragraphs {
                            let is_target = match &visual_data.target_location {
                                StructuralLocation::Docx { global_paragraph_index, .. } => {
                                    *global_paragraph_index == para.original_index
                                },
                                _ => false,
                            };
                            
                            Self::render_paragraph(ui, para, is_target, match_text, pending_scroll);
                        }
                    }
                }
            });
    }

    fn render_paragraph(
        ui: &mut egui::Ui, 
        para: &CachedParagraph, 
        is_target: bool, 
        match_text: &str,
        pending_scroll_target: &mut Option<StructuralLocation>
    ) {
        use egui::text::{LayoutJob, TextFormat};
        use egui::{Color32, FontId, Stroke};

        let mut job = LayoutJob::default();
        job.wrap.max_width = ui.available_width();
        let normal_format = TextFormat {
            font_id: FontId::proportional(14.0),
            color: ui.visuals().text_color(),
            ..Default::default()
        };

        if is_target {
            let highlight_format = TextFormat {
                font_id: FontId::proportional(14.0),
                color: Color32::BLACK,
                background: Color32::from_rgb(250, 210, 80), 
                underline: Stroke::new(1.5, Color32::BLACK), 
                ..Default::default()
            };

            if let Some(start_idx) = para.text.to_lowercase().find(&match_text.to_lowercase()) {
                let end_idx = start_idx + match_text.len();

                let head = &para.text[0..start_idx];
                let matched_sub = &para.text[start_idx..end_idx];
                let tail = &para.text[end_idx..];

                job.append(head, 0.0, normal_format.clone());
                job.append(matched_sub, 0.0, highlight_format);
                job.append(tail, 0.0, normal_format);
            } else {
                job.append(&para.text, 0.0, highlight_format);
            }
        } else {
            job.append(&para.text, 0.0, normal_format);
        }

        let response = ui.label(job);

        if is_target {
            if let Some(_target) = pending_scroll_target.take() {
                response.scroll_to_me(Some(egui::Align::Center));
            }
        }

        ui.add_space(8.0);
    }
}

impl eframe::App for TraceTextGui {
    // Implementing the updated 0.34.3 signature
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        
        // 1. Process background thread responses
        if let Ok(response) = self.rx_response.try_recv() {
            self.pending_scroll_target = Some(response.target_location.clone());
            self.active_visualization = Some(response);
        }

        let screen_width = ui.available_width();
        let window_height = ui.available_height();
        let left_proportion = 0.65;
        
        ui.columns(2, |columns| {

            columns[0].vertical(|ui| {
                egui::ScrollArea::vertical()
                    .id_salt("left_controls_scroll") // Required in 0.34+ for unique scroll areas
                    .show(ui, |ui| {
                    // 2. Left Column (Content)
                    // By setting a max_size relative to the screen, we guarantee the right 
                    // column ALWAYS has a minimum amount of space (e.g., 300px) to render safely.
                    egui::Panel::left("master_panel")
                        .resizable(true)
                        .min_size(400.0) 
                        .max_size(screen_width - 300.0) // Prevents crushing the visualizer
                        .default_size(screen_width * left_proportion)
                        .show_inside(ui, |ui| { 
                            
                            // Vertical split - Top half for inputs
                            egui::Panel::top("inputs_panel")
                                .resizable(true)
                                .min_size(window_height * 0.4)
                                .show_inside(ui, |ui| {
                                    egui::ScrollArea::vertical().show(ui, |ui| {
                                        ui.heading("TraceText - Búsqueda Estructural de Documentos");
                                        ui.add_space(10.0);

                                        ui.horizontal_wrapped(|ui| {
                                            if ui.button("📁 Seleccionar Documento PDF/Word").clicked() {
                                                if let Some(path) = FileDialog::new()
                                                    .add_filter("Documentos", &["pdf", "docx"])
                                                    .pick_file() {
                                                    self.file_path = Some(path);
                                                    self.status_message = "Archivo cargado.".into();
                                                }
                                            }
                                            if let Some(path) = &self.file_path {
                                                ui.add(egui::Label::new(path.display().to_string()).truncate());
                                            } else {
                                                ui.label("Ningún archivo seleccionado.");
                                            }
                                        });
                                        ui.add_space(10.0);

                                        ui.label("Consultas de Búsqueda (Una por línea):");
                                        ui.add(egui::TextEdit::multiline(&mut self.queries_text).desired_rows(5).desired_width(f32::INFINITY));
                                        ui.add_space(10.0);

                                        ui.horizontal_wrapped(|ui| {
                                            ui.add(egui::Slider::new(&mut self.threshold, 0.0..=100.0).text("Umbral (%)"));
                                            ui.add_space(10.0);
                                            ui.add(egui::Slider::new(&mut self.buffer_size, 10..=200).text("Búfer de Contexto"));
                                            ui.add_space(10.0);
                                            ui.add(egui::Slider::new(&mut self.display_limit, 50..=500).text("Límite de Visualización"));
                                        });
                                        ui.add_space(10.0);

                                        ui.horizontal_wrapped(|ui| {
                                            if ui.button("🔍 Ejecutar Búsqueda").clicked() {
                                                if let Some(path) = &self.file_path {
                                                    let queries: Vec<CompactString> = self.queries_text
                                                        .lines()
                                                        .filter(|l| !l.trim().is_empty())
                                                        .map(|l| CompactString::from(l.trim()))
                                                        .collect();

                                                    match TraceTextApp::run_search(path, queries, self.threshold, self.buffer_size, self.display_limit) {
                                                        Ok(res) => {
                                                            self.results = res;
                                                            self.status_message = format!("Se encontraron {} filas de resultados.", self.results.len());
                                                        }
                                                        Err(e) => {
                                                            self.status_message = format!("Error: {}", e);
                                                        }
                                                    }
                                                } else {
                                                    self.status_message = "Error: Por favor seleccione un documento primero.".into();
                                                }
                                            }
                                            ui.add(egui::Label::new(
                                                egui::RichText::new(&self.status_message).color(egui::Color32::DARK_GRAY)
                                            ).truncate());
                                        });
                                        ui.add_space(10.0);
                                    });
                                });

                            // Vertical split - Bottom half for the Table
                            // CentralPanel cleanly consumes exactly the vertical space left over by the Top panel
                            egui::CentralPanel::default().show_inside(ui, |ui| {
                                if !self.results.is_empty() {
                                    ui.horizontal(|ui| {
                                        if ui.button("📋 Copiar Tabla").clicked() {
                                            let tsv = format_clipboard_tsv(&self.results);
                                            ui.ctx().copy_text(tsv); 
                                            self.status_message = "¡Tabla copiada al portapapeles!".into();
                                        }

                                        if ui.button("📊 Exportar a Excel").clicked() {
                                            if let Some(save_path) = FileDialog::new()
                                                .add_filter("Libro de Excel", &["xlsx"])
                                                .set_file_name("TraceText_Resultados.xlsx")
                                                .save_file() {
                                                match export_to_excel(&self.results, &save_path) {
                                                    Ok(_) => self.status_message = "¡Exportación exitosa!".into(),
                                                    Err(e) => self.status_message = format!("Fallo en la exportación: {}", e),
                                                }
                                            }
                                        }
                                    });
                                    ui.add_space(10.0);

                                    TableBuilder::new(ui)
                                        .striped(true)
                                        .resizable(true)
                                        .sense(egui::Sense::click())
                                        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                                        .column(Column::initial(200.0).clip(true))
                                        .column(Column::initial(70.0).clip(true))
                                        .column(Column::remainder().clip(true))
                                        .column(Column::initial(100.0).clip(true))
                                        .header(20.0, |mut header| {
                                            header.col(|ui| { ui.strong("Consulta"); });
                                            header.col(|ui| { ui.strong("Puntuación"); });
                                            header.col(|ui| { ui.strong("Texto Original"); });
                                            header.col(|ui| { ui.strong("Ubicación"); });
                                        })
                                        .body(|mut body| {
                                            for (index, row) in self.results.iter().enumerate() {
                                                let is_selected = Some(index) == self.selected_row_index;
                                                
                                                body.row(25.0, |mut ui_row| {
                                                    ui_row.set_selected(is_selected);
                                                    
                                                    let mut row_interacted = false;
                                                    ui_row.col(|ui| { 
                                                        if ui.add(egui::Label::new(&row.query).sense(egui::Sense::click())).clicked() { row_interacted = true; }
                                                    });
                                                    ui_row.col(|ui| { 
                                                        if ui.add(egui::Label::new(format!("{:.2}", row.score)).sense(egui::Sense::click())).clicked() { row_interacted = true; }
                                                    });
                                                    ui_row.col(|ui| {
                                                        if row.score > 0.0 {
                                                            let mut job = egui::text::LayoutJob::default();
                                                            job.wrap.max_width = ui.available_width();
                                                            let font_id = egui::TextStyle::Body.resolve(ui.style());
                                                            let text_color = ui.visuals().text_color();
                                                            let match_color = ui.visuals().strong_text_color();

                                                            job.append(&row.prefix, 0.0, egui::TextFormat { font_id: font_id.clone(), color: text_color, ..Default::default() });
                                                            
                                                            let is_perfect = row.score >= 99.9;
                                                            let highlight_format = if is_perfect {
                                                                egui::TextFormat { font_id: font_id.clone(), color: match_color, underline: egui::Stroke::new(1.5, match_color), ..Default::default() }
                                                            } else {
                                                                egui::TextFormat { font_id: font_id.clone(), color: match_color, ..Default::default() }
                                                            };
                                                            
                                                            job.append(&row.match_text, 0.0, highlight_format);
                                                            job.append(&row.suffix, 0.0, egui::TextFormat { font_id, color: text_color, ..Default::default() });

                                                            if ui.add(egui::Label::new(job).sense(egui::Sense::click())).clicked() { row_interacted = true; }
                                                        } else {
                                                            if ui.add(egui::Label::new(&row.prefix).sense(egui::Sense::click())).clicked() { row_interacted = true; }
                                                        }
                                                    });
                                                    ui_row.col(|ui| { 
                                                        if ui.add(egui::Label::new(&row.location).sense(egui::Sense::click())).clicked() { row_interacted = true; }
                                                    });
                                                    
                                                    if ui_row.response().clicked() || row_interacted {
                                                        self.selected_row_index = Some(index);
                                                        
                                                        if let (Some(path), Some(loc)) = (&self.file_path, &row.raw_location) {
                                                            self.active_match_text = Some(row.match_text.clone());
                                                            
                                                            let cached_doc = {
                                                                let r_lock = self.doc_cache.read().unwrap();
                                                                r_lock.get(path).cloned()
                                                            };

                                                            if let Some(doc) = cached_doc {
                                                                self.pending_scroll_target = Some(loc.clone());
                                                                self.active_visualization = Some(ParserResponse { file_path: path.clone(), document: doc, target_location: loc.clone() });
                                                            } else {
                                                                let _ = self.tx_request.send(ParserRequest { file_path: path.clone(), target_location: loc.clone() });
                                                            }
                                                        }
                                                    }
                                                });
                                            }
                                        });
                                }
                            });
                        });
                    });
            });
            columns[1].vertical(|ui| {
                // 3. Right Column (Visualizer)
                // Use ui.global_style().visuals.panel_fill to perfectly match the app's native background
                let visualizer_frame = egui::Frame::NONE
                    .fill(ui.global_style().visuals.panel_fill) 
                    .inner_margin(16.0);

                // CentralPanel anchored directly to `ui` automatically consumes exactly 
                // what Panel leaves behind without breaking the layout engine.
                egui::CentralPanel::default()
                    .frame(visualizer_frame)
                    .show_inside(ui, |ui| {
                        let detail_width = ui.available_width();
                        let header_text = if detail_width < 320.0 {
                            "Contexto"
                        } else if detail_width < 480.0 {
                            "Visualizador de Contexto"
                        } else {
                            "Visualizador de Contexto Completo"
                        };

                        ui.heading(header_text);
                        ui.separator();

                        self.draw_context_visualizer(ui);
                    });
            });
        });
    }
}

// --- Helper Functions ---

fn format_clipboard_tsv(results: &[DisplayRow]) -> String {
    let mut tsv = String::from("Consulta\tPuntuación\tTexto Original Coincidente\tUbicación\n");
    for r in results {
        tsv.push_str(&format!("{}\t{:.2}\t{}\t{}\n", r.query, r.score, r.full_text(), r.location));
    }
    tsv
}

fn export_to_excel(results: &[DisplayRow], path: &Path) -> Result<()> {
    let mut workbook = Workbook::new();
    let worksheet = workbook.add_worksheet();

    worksheet.write_string(0, 0, "Consulta")?;
    worksheet.write_string(0, 1, "Puntuación")?;
    worksheet.write_string(0, 2, "Texto Original Coincidente")?;
    worksheet.write_string(0, 3, "Ubicación")?;

    for (row_idx, row) in results.iter().enumerate() {
        let r = (row_idx + 1) as u32;
        worksheet.write_string(r, 0, &row.query)?;
        worksheet.write_number(r, 1, row.score as f64)?;
        worksheet.write_string(r, 2, &row.full_text())?;
        worksheet.write_string(r, 3, &row.location)?;
    }

    workbook.save(path)?;
    Ok(())
}

pub fn resolve_highlight(
    raw_text: &str, 
    indices: &[u32]
) -> (CompactString, CompactString, CompactString) {
    if indices.is_empty() {
        return (
            CompactString::from(raw_text),
            CompactString::default(),
            CompactString::default(),
        );
    }

    let start_char_idx = *indices.iter().min().unwrap() as usize;
    let end_char_idx = *indices.iter().max().unwrap() as usize;

    let mut start_byte = 0;
    let mut end_byte = raw_text.len();
    let mut current_char_idx = 0;

    for (byte_idx, c) in raw_text.char_indices() {
        if current_char_idx == start_char_idx {
            start_byte = byte_idx;
        }
        if current_char_idx == end_char_idx {
            end_byte = byte_idx + c.len_utf8();
            break; 
        }
        current_char_idx += 1;
    }

    let prefix = CompactString::from(&raw_text[..start_byte]);
    let match_text = CompactString::from(&raw_text[start_byte..end_byte]);
    let suffix = CompactString::from(&raw_text[end_byte..]);

    (prefix, match_text, suffix)
}

fn apply_buffer(text: &str, buffer_size: usize, is_prefix: bool) -> CompactString {
    let char_count = text.chars().count();
    if char_count <= buffer_size {
        return CompactString::from(text);
    }
    
    if is_prefix {
        let skip = char_count - buffer_size;
        let truncated: String = text.chars().skip(skip).collect();
        CompactString::from(format!("...{}", truncated))
    } else {
        let truncated: String = text.chars().take(buffer_size).collect();
        CompactString::from(format!("{}...", truncated))
    }
}

// --- Visualizer Models ---

#[derive(Debug, Clone)]
pub struct CachedParagraph {
    pub text: String,
    pub original_index: usize,
    pub is_heading: bool,
    pub heading_level: Option<usize>,
}

#[derive(Debug, Clone)]
pub enum CachedDocument {
    Pdf {
        pages: Vec<Vec<CachedParagraph>>,
    },
    Docx {
        paragraphs: Vec<CachedParagraph>,
    },
}

pub struct ParserRequest {
    pub file_path: PathBuf,
    pub target_location: StructuralLocation,
}

pub struct ParserResponse {
    pub file_path: PathBuf,
    pub document: CachedDocument,
    pub target_location: StructuralLocation,
}

fn parse_document_by_type(path: &Path) -> CachedDocument {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    
    if ext == "pdf" {
        let mut pages = Vec::new();
        if let Ok(parser) = PdfParser::open(path) {
            let _ = parser.for_each_page(PageStreamOptions::default(), |event| {
                if let ParseEvent::PageParsed(page) = event {
                    let mut paragraphs = Vec::new();
                    for (elem_idx, element) in page.elements.iter().enumerate() {
                        let mut text = String::new();
                        element.append_plain_text(&mut text);
                        if !text.trim().is_empty() {
                            paragraphs.push(CachedParagraph {
                                text,
                                original_index: elem_idx, 
                                is_heading: false,
                                heading_level: None,
                            });
                        }
                    }
                    pages.push(paragraphs);
                }
                ControlFlow::Continue(())
            });
        }
        CachedDocument::Pdf { pages }
    } else if ext == "docx" {
        let mut paragraphs = Vec::new();
        let mut global_para_count = 0;
        if let Ok(mut parser) = DocxParser::open(path) {
            if let Ok(doc) = parser.parse() {
                for section in &doc.sections {
                    for block in &section.content {
                        let text = match block {
                            undoc::Block::Paragraph(para) => para.plain_text(),
                            undoc::Block::Table(table) => table.plain_text(),
                            _ => String::new(),
                        };
                        if !text.trim().is_empty() {
                            global_para_count += 1;
                            paragraphs.push(CachedParagraph {
                                text,
                                original_index: global_para_count, 
                                is_heading: false, 
                                heading_level: None,
                            });
                        }
                    }
                }
            }
        }
        CachedDocument::Docx { paragraphs }
    } else {
        CachedDocument::Docx { paragraphs: vec![] }
    }
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
        Box::new(|cc| Ok(Box::new(TraceTextGui::new(cc)))),
    )
}
