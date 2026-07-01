use eframe::egui;
use egui_extras::{Column, TableBuilder, StripBuilder, Size};
use std::{collections::{HashMap, VecDeque}, path::PathBuf, sync::{mpsc::{channel, Receiver, Sender}, Arc, RwLock}};
use egui::{ColorImage, TextureHandle, TextureOptions};
use crate::app::DisplayRow;
use crate::extractor::DocumentExtractor;
use crate::models::{ParserRequest, ParserResponse, BBox};
use crate::palette::{Palette, ThemeMode};

/// Dynamic cache housing the GPU textures and coordinate configurations for the active document[cite: 106].
pub struct PageRenderCache {
    #[allow(dead_code)]
    pub page_index: usize,
    pub texture: TextureHandle,
    pub page_width_points: f32,
    pub page_height_points: f32,
}

/// A lightweight LRU cache to manage GPU memory and prevent VRAM exhaustion[cite: 171].
/// Maintains only the currently viewed page and its immediate neighbors in memory[cite: 174].
pub struct TextureLruCache {
    capacity: usize,
    cache: HashMap<usize, PageRenderCache>,
    order: VecDeque<usize>, // Tracks access history
}

impl TextureLruCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            cache: HashMap::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
        }
    }

    pub fn get(&mut self, page_index: usize) -> Option<&PageRenderCache> {
        if self.cache.contains_key(&page_index) {
            // Update access order to mark as most recently used
            self.order.retain(|&idx| idx != page_index);
            self.order.push_back(page_index);
            self.cache.get(&page_index)
        } else {
            None
        }
    }

    pub fn insert(&mut self, page_index: usize, render_cache: PageRenderCache) {
        if !self.cache.contains_key(&page_index) {
            if self.cache.len() >= self.capacity {
                // Evict the least recently used page texture
                if let Some(lru_index) = self.order.pop_front() {
                    // When removed from the HashMap, the TextureHandle is dropped.
                    // Egui automatically garbage collects the GPU texture[cite: 56, 175].
                    self.cache.remove(&lru_index);
                }
            }
        } else {
            self.order.retain(|&idx| idx != page_index);
        }
        
        self.order.push_back(page_index);
        self.cache.insert(page_index, render_cache);
    }

    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.cache.clear();
        self.order.clear();
    }
}

pub struct TraceTextGui {
    theme_mode: ThemeMode,
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
    #[allow(dead_code)]
    doc_cache: Arc<RwLock<HashMap<PathBuf, Vec<u8>>>>,
    pub texture_cache: TextureLruCache,
    pub active_page_index: Option<usize>,
    pub pending_scroll_target: Option<Vec<BBox>>,
    #[allow(dead_code)]
    pub active_match_text: Option<String>,
}

impl TraceTextGui {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let doc_cache = Arc::new(RwLock::new(HashMap::new()));
        
        let (tx_request, rx_request) = channel::<ParserRequest>();
        let (tx_response, rx_response) = channel::<ParserResponse>();
        
        let doc_cache_clone = Arc::clone(&doc_cache);
        let egui_ctx = cc.egui_ctx.clone();

        std::thread::spawn(move || {
            use pdfium_render::prelude::*;
            
            // 1. Isolate pdfium-render safely inside this background worker.
            let bindings = Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path("./"))
                .or_else(|_| Pdfium::bind_to_system_library())
                .expect("Critical Error: Failed to bind to local or system Pdfium binaries.");
            
            let pdfium = Pdfium::new(bindings);
            let extractor = DocumentExtractor;

            while let Ok(request) = rx_request.recv() {
                match request {
                    // Phase 2: Ingestion & Spatial Index Generation
                    ParserRequest::IngestDocument { file_path } => {
                        let unified_pdf_bytes = match extractor.ingest_to_pdf_bytes(&file_path) {
                            Ok(bytes) => bytes,
                            Err(e) => {
                                let _ = tx_response.send(ParserResponse::Error(e.to_string()));
                                continue;
                            }
                        };

                        // Build the SpatialIndex mapping NFD characters to 2D physical bounding boxes
                        match extractor.build_spatial_index(&pdfium, &unified_pdf_bytes) {
                            Ok(spatial_index) => {
                                let mut w_lock = doc_cache_clone.write().unwrap();
                                w_lock.insert(file_path.clone(), unified_pdf_bytes);
                                
                                let _ = tx_response.send(ParserResponse::DocumentIndexed {
                                    file_path,
                                    spatial_index,
                                });
                                egui_ctx.request_repaint();
                            }
                            Err(e) => {
                                let _ = tx_response.send(ParserResponse::Error(format!("Index mapping failed: {}", e)));
                            }
                        }
                    }

                    // Phase 2: Background Rasterization 
                    ParserRequest::FetchPage { file_path, page_index, target_width_px } => {
                        let cache_read = {
                            let r_lock = doc_cache_clone.read().unwrap();
                            r_lock.get(&file_path).cloned()
                        };

                        if let Some(pdf_bytes) = cache_read {
                            if let Ok(document) = pdfium.load_pdf_from_byte_slice(&pdf_bytes, None) {
                                if let Ok(page) = document.pages().get(page_index as i32) {
                                    
                                    let page_width_points = page.width().value;
                                    let page_height_points = page.height().value;
                                    
                                    let aspect_ratio = page_height_points / page_width_points;
                                    let target_height_px = (target_width_px as f32 * aspect_ratio) as i32;

                                    let render_config = PdfRenderConfig::new()
                                        .set_target_width(target_width_px)
                                        .set_maximum_height(target_height_px)
                                        .render_annotations(true)
                                        .render_form_data(true);

                                    if let Ok(rendered_page) = page.render_with_config(&render_config) {
                                        if let Ok(dynamic_image) = rendered_page.as_image() {
                                            let rgba_image = dynamic_image.into_rgba8();
                                            
                                            let _ = tx_response.send(ParserResponse::PageImage {
                                                file_path,
                                                page_index,
                                                width_px: rgba_image.width() as usize,
                                                height_px: rgba_image.height() as usize,
                                                rgba_buffer: rgba_image.into_raw(),
                                                page_width_points,
                                                page_height_points,
                                            });
                                            egui_ctx.request_repaint();
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });

        Self {
            theme_mode: ThemeMode::System,
            file_path: None,
            queries_text: "".to_string(),
            threshold: 85.0,
            buffer_size: 60,
            display_limit: 200,
            results: Vec::new(),
            status_message: "Listo. Inicie cargando un documento de origen para el emparejamiento.".into(),
            selected_row_index: None,
            tx_request,
            rx_response,
            doc_cache,
            texture_cache: TextureLruCache::new(3),
            active_page_index: None,
            pending_scroll_target: None,
            active_match_text: None,
        }
    }

    /// Configures the color palette and active theme context.
    fn configure_visuals(&self, ctx: &egui::Context) -> Palette {
        let is_dark = match self.theme_mode {
            ThemeMode::System => ctx.system_theme().map(|t| t == egui::Theme::Dark).unwrap_or(true),
            ThemeMode::Light => false,
            ThemeMode::Dark => true,
        };
        let pal = Palette::current(is_dark);

        let mut visuals = if is_dark { egui::Visuals::dark() } else { egui::Visuals::light() };
        
        visuals.window_fill = pal.base_bg;
        visuals.panel_fill = pal.panel_bg;
        visuals.extreme_bg_color = pal.input_bg;
        visuals.faint_bg_color = pal.row_alt;

        visuals.widgets.noninteractive.fg_stroke.color = pal.primary_text;
        visuals.widgets.inactive.fg_stroke.color = pal.primary_text; 
        visuals.widgets.noninteractive.bg_fill = pal.panel_bg;

        visuals.widgets.inactive.bg_fill = pal.idle_ctrl;
        visuals.widgets.hovered.bg_fill = pal.hover_ctrl;
        visuals.widgets.hovered.fg_stroke.color = pal.strong_text;
        visuals.widgets.active.bg_fill = pal.active_ctrl;
        visuals.widgets.active.fg_stroke.color = pal.strong_text;

        visuals.selection.bg_fill = pal.row_sel;

        ctx.set_visuals(visuals);
        pal
    }

    /// SECCIÓN 1: Panel de Control y Configuración (Columna Izquierda Superior)
    fn draw_control_panel(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        egui::ScrollArea::vertical()
            .id_salt("inputs_control_scroll")
            .show(ui, |ui| {
                ui.label(egui::RichText::new("PANEL DE CONTROL Y CONFIGURACIÓN").strong().size(12.0).color(pal.strong_text));
                ui.separator();
                ui.add_space(8.0);

                ui.label(egui::RichText::new("TEMA DE INTERFAZ").strong().size(10.0).color(pal.strong_text));
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.theme_mode, ThemeMode::System, "Sistema");
                    ui.radio_value(&mut self.theme_mode, ThemeMode::Light, "Claro");
                    ui.radio_value(&mut self.theme_mode, ThemeMode::Dark, "Oscuro");
                });
                ui.add_space(8.0);

                ui.label(egui::RichText::new("ORIGEN DE DATOS").strong().size(10.0).color(pal.strong_text));
                ui.horizontal_wrapped(|ui| {
                    let select_btn = ui.add(egui::Button::new(egui::RichText::new("📁 Seleccionar Documento").color(pal.primary_text)).fill(pal.input_bg));
                    if select_btn.clicked() {
                        if let Some(path) = rfd::FileDialog::new().add_filter("Documentos", &["pdf", "docx"]).pick_file() {
                            // Send the ingestion request to the background worker
                            let _ = self.tx_request.send(crate::models::ParserRequest::IngestDocument { 
                                file_path: path.clone() 
                            });
                            
                            self.file_path = Some(path);
                            self.status_message = "Archivo importado. Generando índice espacial...".into();
                        }
                    }

                    if let Some(path) = &self.file_path {
                        ui.label(egui::RichText::new(path.display().to_string()).monospace().color(pal.primary_text));
                    } else {
                        ui.label(egui::RichText::new("Ningún documento seleccionado.").italics().color(pal.subdued_text));
                    }
                });
                ui.add_space(8.0);

                ui.label(egui::RichText::new("PARÁMETROS DE BÚSQUEDA").strong().size(10.0).color(pal.strong_text));
                ui.add(egui::Slider::new(&mut self.threshold, 0.0..=100.0).text("Umbral (%)"));
                ui.add_space(4.0);
                ui.add(egui::Slider::new(&mut self.buffer_size, 10..=200).text("Búfer"));
                ui.add_space(4.0);
                ui.add(egui::Slider::new(&mut self.display_limit, 50..=500).text("Límite"));
                ui.add_space(8.0);

                ui.horizontal_wrapped(|ui| {
                    let search_btn = ui.add(egui::Button::new(egui::RichText::new("🔍 Ejecutar Búsqueda").strong().color(pal.primary_text)).fill(pal.input_bg));
                    if search_btn.clicked() {
                        if let Some(path) = &self.file_path {
                            let queries: Vec<compact_str::CompactString> = self.queries_text
                                .lines()
                                .filter(|l| !l.trim().is_empty())
                                .map(|l| compact_str::CompactString::from(l.trim()))
                                .collect();
                            if queries.is_empty() {
                                self.status_message = "Error: Ingrese al menos una consulta válida.".into();
                            } else {
                                match crate::app::TraceTextApp::run_search(path, queries, self.threshold, self.buffer_size, self.display_limit) {
                                    Ok(res) => {
                                        self.results = res;
                                        self.status_message = format!("Búsqueda completada. Se generaron {} resultados.", self.results.len());
                                        self.selected_row_index = None;
                                    }
                                    Err(e) => {
                                        self.status_message = format!("Fallo del motor: {}", e);
                                    }
                                }
                            }
                        } else {
                            self.status_message = "Error: Cargue un archivo antes de procesar la búsqueda.".into();
                        }
                    }

                    if ui.add(egui::Button::new(egui::RichText::new("📊 Excel").color(pal.primary_text)).fill(pal.input_bg)).clicked() {
                        if self.results.is_empty() {
                            self.status_message = "Error: No hay datos en la tabla.".into();
                        } else if let Some(save_path) = rfd::FileDialog::new().add_filter("Excel", &["xlsx"]).set_file_name("Resultados.xlsx").save_file() {
                            match crate::utils::export_to_excel(&self.results, &save_path) {
                                Ok(_) => self.status_message = "Exportación completada.".into(),
                                Err(e) => self.status_message = format!("Fallo en exportación: {}", e),
                            }
                        }
                    }

                    if ui.add(egui::Button::new(egui::RichText::new("📋 Copiar").color(pal.primary_text)).fill(pal.input_bg)).clicked() {
                        if !self.results.is_empty() {
                            ui.ctx().copy_text(crate::utils::format_clipboard_tsv(&self.results));
                            self.status_message = "Transferido al portapapeles.".into();
                        }
                    }
                });
                ui.add_space(8.0);

                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("Estado:").size(11.0).color(pal.strong_text));
                    ui.label(egui::RichText::new(&self.status_message).size(11.0).color(pal.primary_text));
                });
            });
    }

    /// SECCIÓN 2: Panel de Consultas Multilínea (Columna Izquierda Inferior)
    fn draw_queries_panel(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        // Unique identifiers for state tracking and temporary caching
        let text_edit_id = egui::Id::new("queries_text_edit");
        let cache_id = egui::Id::new("queries_text_edit_cache");

        ui.vertical(|ui| {
            // Apply visual configurations matching the application palette
            ui.visuals_mut().extreme_bg_color = pal.input_bg;
            ui.visuals_mut().selection.bg_fill = pal.row_sel;
            //ui.visuals_mut().selection.stroke = egui::Stroke::new(1.0, pal.active_ctrl);

            // Construct the multiline TextEdit widget matching existing layouts
            let text_edit = egui::TextEdit::multiline(&mut self.queries_text)
               .id(text_edit_id)
               .desired_width(f32::INFINITY)
               .desired_rows(15)
               .font(egui::TextStyle::Monospace)
               .hint_text("Escriba sus consultas aquí (una por línea)...");

            // Render the TextEdit widget and capture its layout output metadata
            let output = text_edit.show(ui);

            // Detect if a secondary pointer action (right-click) is occurring within the widget
            let is_secondary_down = ui.input(|i| i.pointer.button_down(egui::PointerButton::Secondary));
            let is_secondary_pressed = ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Secondary));
            let has_secondary_interaction = (is_secondary_down || is_secondary_pressed) && output.response.hovered();

            // Cache the active selection range only on standard frames
            if let Some(current_range) = output.cursor_range {
                if!has_secondary_interaction {
                    ui.ctx().data_mut(|d| d.insert_temp(cache_id, current_range));
                }
            }

            // Restore selection state during secondary click frames to prevent default collapse
            if has_secondary_interaction {
                if let Some(cached_range) = ui.ctx().data(|d| d.get_temp::<egui::text::CCursorRange>(cache_id)) {
                    let mut state = egui::widgets::text_edit::TextEditState::load(ui.ctx(), text_edit_id)
                       .unwrap_or_default();
                    state.cursor.set_char_range(Some(cached_range));
                    state.store(ui.ctx(), text_edit_id);
                }
            }

            // Bind the context menu handler to the TextEdit response
            output.response.context_menu(|ui| {
                // Request focus to keep the underlying TextEdit rendering visual highlights
                ui.ctx().memory_mut(|mem| mem.request_focus(text_edit_id));

                // Retrieve the preserved character selection range from cache
                let cached_range: Option<egui::text::CCursorRange> = ui.ctx().data(|d| d.get_temp(cache_id));

                // Determine if a non-zero character selection was active
                let has_selection = if let Some(range) = cached_range {
                    range.primary!= range.secondary
                } else {
                    false
                };

                // Bug 1 Resolution: Show "📋 Copiar" only when an active selection range exists
                if has_selection {
                    if ui.button("📋 Copiar").clicked() {
                        if let Some(range) = cached_range {
                            let char_range = range.as_sorted_char_range();
                            
                            // Safe Unicode character collection to safeguard UTF-8 boundaries
                            let selected_text: String = self.queries_text
                               .chars()
                               .skip(char_range.start)
                               .take(char_range.end - char_range.start)
                               .collect();

                            if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                let _ = clipboard.set_text(selected_text);
                            }
                        }
                        ui.close();
                    }
                }

                // Copy all text option is always available
                if ui.button("📋 Copiar Todo").clicked() {
                    if let Ok(mut clipboard) = arboard::Clipboard::new() {
                        let _ = clipboard.set_text(self.queries_text.clone());
                    }
                    ui.close();
                }

                // Bug 2 Resolution: "📋 Pegar" with cursor caret insertion and selection replacement
                if ui.button("📋 Pegar").clicked() {
                    if let Ok(mut clipboard) = arboard::Clipboard::new() {
                        if let Ok(pasted_text) = clipboard.get_text() {
                            if let Some(range) = cached_range {
                                let char_range = range.as_sorted_char_range();

                                // Character-level slicing to avoid multi-byte UTF-8 splitting
                                let before: String = self.queries_text.chars().take(char_range.start).collect();
                                let after: String = self.queries_text.chars().skip(char_range.end).collect();

                                // Splicing sequence concatenation
                                self.queries_text = format!("{}{}{}", before, pasted_text, after);

                                // Compute the new caret offset following the inserted text
                                let pasted_char_count = pasted_text.chars().count();
                                let new_cursor_pos = char_range.start + pasted_char_count;

                                // Update the TextEditState to focus the new caret position
                                let mut state = egui::widgets::text_edit::TextEditState::load(ui.ctx(), text_edit_id)
                                   .unwrap_or_default();
                                let new_ccursor = egui::text::CCursor::new(new_cursor_pos);
                                let new_range = egui::text::CCursorRange::one(new_ccursor);
                                state.cursor.set_char_range(Some(new_range));
                                state.store(ui.ctx(), text_edit_id);

                                // Update the cache to synchronize with the new caret range
                                ui.ctx().data_mut(|d| d.insert_temp(cache_id, new_range));
                            } else {
                                // Fallback: Append text to the end of the buffer if no selection state exists
                                self.queries_text.push_str(&pasted_text);
                            }
                        }
                    }
                    ui.close();
                }

                // Spanish localized "🗑 Limpiar" button clears the buffer safely
                if ui.button("🗑 Limpiar").clicked() {
                    self.queries_text.clear();

                    // Re-initialize the selection state back to index 0
                    let mut state = egui::widgets::text_edit::TextEditState::load(ui.ctx(), text_edit_id)
                       .unwrap_or_default();
                    let zero_ccursor = egui::text::CCursor::new(0);
                    let zero_range = egui::text::CCursorRange::one(zero_ccursor);
                    state.cursor.set_char_range(Some(zero_range));
                    state.store(ui.ctx(), text_edit_id);

                    ui.ctx().data_mut(|d| d.insert_temp(cache_id, zero_range));
                    ui.close();
                }
            });
        });
    }

    /// SECCIÓN 3: Tabla de Resultados Completa (Columna Derecha Superior)
    fn draw_results_table(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        ui.label(egui::RichText::new("TABLA DE RESULTADOS").strong().size(12.0).color(pal.strong_text));
        ui.separator();
        ui.add_space(4.0);

        if self.results.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(egui::RichText::new("Sin coincidencias activas. Defina las consultas y ejecute la búsqueda.").italics().color(pal.subdued_text));
            });
        } else {
            ui.scope(|ui| {
                ui.style_mut().visuals.widgets.hovered.bg_fill = pal.row_hover;
                
                TableBuilder::new(ui)
                    .id_salt("results_grid")
                    .striped(true)
                    .resizable(true)
                    .sense(egui::Sense::click())
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::initial(110.0).at_least(80.0).clip(true))
                    .column(Column::initial(70.0).at_least(60.0).clip(true))
                    .column(Column::remainder().clip(true).resizable(false))
                    .column(Column::initial(120.0).at_least(90.0).clip(true))
                    .header(22.0, |mut header| {
                        header.col(|ui| { ui.label(egui::RichText::new("Consulta").color(pal.strong_text).strong()); });
                        header.col(|ui| { ui.label(egui::RichText::new("Puntuación").color(pal.strong_text).strong()); });
                        header.col(|ui| { ui.label(egui::RichText::new("Texto Encontrado").color(pal.strong_text).strong()); });
                        header.col(|ui| { ui.label(egui::RichText::new("Ubicación").color(pal.strong_text).strong()); });
                    })
                    .body(|mut body| {
                        for (index, row) in self.results.iter().enumerate() {
                            let is_selected = Some(index) == self.selected_row_index;
                            
                            body.row(26.0, |mut ui_row| {
                                ui_row.set_selected(is_selected);
                                let mut row_interacted = false;
                                
                                ui_row.col(|ui| { if ui.add(egui::Label::new(format!("Página {}", row.page_number)).sense(egui::Sense::click())).clicked() { row_interacted = true; }});
                                ui_row.col(|ui| {
                                    if ui.add(egui::Label::new(egui::RichText::new(format!("{:.2}", row.score)).strong()).sense(egui::Sense::click())).clicked() { row_interacted = true; }
                                });
                                ui_row.col(|ui| {
                                    if row.score > 0.0 {
                                        let mut job = egui::text::LayoutJob::default();
                                        job.wrap.max_width = ui.available_width();
                                        let font_id = egui::TextStyle::Body.resolve(ui.style());

                                        job.append(&row.prefix, 0.0, egui::TextFormat { font_id: font_id.clone(), color: pal.primary_text, ..Default::default() });
                                        
                                        let highlight_format = egui::TextFormat { 
                                            font_id: font_id.clone(), 
                                            color: pal.match_fg, 
                                            background: pal.match_bg,
                                            ..Default::default() 
                                        };
                                        
                                        job.append(&row.match_text, 0.0, highlight_format);
                                        job.append(&row.suffix, 0.0, egui::TextFormat { font_id, color: pal.primary_text, ..Default::default() });

                                        if ui.add(egui::Label::new(job).sense(egui::Sense::click())).clicked() { row_interacted = true; }
                                    } else {
                                        if ui.add(egui::Label::new(&row.prefix).sense(egui::Sense::click())).clicked() { row_interacted = true; }
                                    }
                                });
                                ui_row.col(|ui| { if ui.add(egui::Label::new(format!("Página {}", row.page_number)).sense(egui::Sense::click())).clicked() { row_interacted = true; }});
                                
                                if ui_row.response().clicked() || row_interacted {
                                    self.selected_row_index = Some(index);
                                    if let Some(path) = &self.file_path {
                                        // En lugar de guardar una ubicación abstracta, guardamos la página y los BBoxes
                                        self.pending_scroll_target = Some(row.target_highlights.clone());
                                        
                                        // Disparar mensaje al hilo de fondo (background worker) para rasterizar la página específica
                                        let _ = self.tx_request.send(crate::models::ParserRequest::FetchPage { 
                                            file_path: path.clone(), 
                                            page_index: row.page_number, 
                                            target_width_px: 1600 // Resolución de alta fidelidad para la extracción
                                        });
                                    }
                                }
                            });
                        }
                    });
            });
        }
    }

    /// SECCIÓN 4: Visualizador de Documento Completo (Columna Derecha Inferior)
    fn draw_document_visualizer(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        ui.add_space(8.0);
        ui.label(egui::RichText::new("VISUALIZADOR DE DOCUMENTO COMPLETO").strong().size(12.0).color(pal.strong_text));
        ui.separator();
        ui.add_space(4.0);

        // Intentar obtener la caché de la página activa desde el LRU Cache
        let active_page = self.active_page_index;
        let mut cache_hit = false;

        if let Some(page_idx) = active_page {
            if let Some(page_cache) = self.texture_cache.get(page_idx) {
                cache_hit = true;

                egui::ScrollArea::both()
                    .id_salt("visualizer_canvas_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // Determinar el espacio disponible y calcular el tamaño exacto del lienzo
                        let available_width = ui.available_width();
                        let aspect_ratio = page_cache.page_height_points / page_cache.page_width_points;
                        let computed_size = egui::Vec2::new(available_width, available_width * aspect_ratio);

                        // Asignar el lienzo gráfico interactivo
                        let (rect, _response) = ui.allocate_exact_size(computed_size, egui::Sense::hover());

                        if ui.is_rect_visible(rect) {
                            let painter = ui.painter_at(rect);

                            // 1. Dibujar la textura base de la página PDF rasterizada
                            painter.image(
                                page_cache.texture.id(),
                                rect,
                                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                egui::Color32::WHITE,
                            );

                            // 2. Iterar y dibujar las formas de resaltado espacial
                            if let Some(highlights) = &self.pending_scroll_target {
                                
                                // Tonos modernos de alto contraste (naranja corporativo) para destacar coincidencias
                                let highlight_color = egui::Color32::from_rgba_unmultiplied(242, 107, 33, 110);

                                for pdf_box in highlights {
                                    // Proyectar coordenadas físicas a coordenadas de pantalla
                                    let screen_highlight_rect = Self::transform_pdf_to_egui(
                                        pdf_box,
                                        page_cache.page_height_points,
                                        page_cache.page_width_points,
                                        rect,
                                    );

                                    // Crear y dibujar el rectángulo translúcido directamente sobre el lienzo
                                    let highlight_shape = egui::Shape::rect_filled(
                                        screen_highlight_rect,
                                        egui::CornerRadius::ZERO,
                                        highlight_color,
                                    );
                                    painter.add(highlight_shape);
                                }

                                // Auto-scroll inteligente: garantizar que la primera coincidencia esté a la vista
                                if let Some(first_box) = highlights.first() {
                                    let focus_rect = Self::transform_pdf_to_egui(
                                        first_box,
                                        page_cache.page_height_points,
                                        page_cache.page_width_points,
                                        rect,
                                    );
                                    ui.scroll_to_rect(focus_rect, Some(egui::Align::Center));
                                }
                            }
                        }
                    });
            }
        }

        // Estado vacío si no hay textura activa en memoria
        if !cache_hit {
            ui.centered_and_justified(|ui| {
                ui.label(
                    egui::RichText::new("Seleccione una fila en la tabla para cargar y renderizar el lienzo visual.")
                        .italics()
                        .color(pal.subdued_text)
                        .size(14.0)
                );
            });
        }
    }

    /// Proyecta una caja delimitadora física (PostScript) a coordenadas lógicas de pantalla en Egui.
    fn transform_pdf_to_egui(
        pdf_box: &BBox,
        pdf_page_height: f32,
        pdf_page_width: f32,
        ui_rect: egui::Rect,
    ) -> egui::Rect {
        let scale_x = ui_rect.width() / pdf_page_width;
        let scale_y = ui_rect.height() / pdf_page_height;

        // La proyección en X es lineal
        let egui_left = ui_rect.min.x + (pdf_box.left * scale_x);
        let egui_right = ui_rect.min.x + (pdf_box.right * scale_x);

        // La proyección en Y debe invertirse: el origen PostScript es inferior-izquierdo, egui es superior-izquierdo
        let egui_top = ui_rect.min.y + ((pdf_page_height - pdf_box.top) * scale_y);
        let egui_bottom = ui_rect.min.y + ((pdf_page_height - pdf_box.bottom) * scale_y);

        egui::Rect::from_min_max(
            egui::pos2(egui_left, egui_top),
            egui::pos2(egui_right, egui_bottom),
        )
    }

}

impl eframe::App for TraceTextGui {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Channel receiver integration loop
        while let Ok(response) = self.rx_response.try_recv() {
            match response {
                ParserResponse::PageImage {
                    file_path: _,
                    page_index,
                    rgba_buffer,
                    width_px,
                    height_px,
                    page_width_points,
                    page_height_points,
                } => {
                    // Convert the raw RGBA buffer into an egui-compatible ColorImage [cite: 115, 173]
                    let size = [width_px, height_px];
                    let color_image = ColorImage::from_rgba_unmultiplied(size, &rgba_buffer); //[cite: 116];

                    // Allocate the texture to GPU memory [cite: 117]
                    let texture = ui.ctx().load_texture(
                        format!("pdf_page_cache_{}", page_index),
                        color_image,
                        TextureOptions::LINEAR,
                    );

                    // Package into the cache structure
                    let render_cache = PageRenderCache {
                        page_index,
                        texture,
                        page_width_points,
                        page_height_points,
                    };

                    // Insert into LRU Cache. Drops older textures automatically.
                    self.texture_cache.insert(page_index, render_cache);
                    self.active_page_index = Some(page_index);
                }
                ParserResponse::DocumentIndexed { file_path: _, spatial_index: _ } => {
                    // Handle indexing completion (Phase 2/3)
                    self.status_message = "Indexación espacial completada. Listo para búsqueda.".into();
                }
                ParserResponse::Error(err) => {
                    self.status_message = format!("Error del motor: {}", err);
                }
            }
        }

        // Configure layout themes and pull current active color maps
        let pal = self.configure_visuals(ui.ctx());

        egui::Frame::NONE
            .fill(pal.base_bg)
            .inner_margin(egui::Margin::same(16)) 
            .show(ui, |ui| {
                StripBuilder::new(ui)
                    .size(Size::exact(360.0)) // Fixed 360px layout control lane
                    .size(Size::remainder())  
                    .horizontal(|mut main_strip| {
                        
                        // COLUMNA IZQUIERDA
                        main_strip.cell(|ui| {
                            StripBuilder::new(ui)
                                .size(Size::exact(300.0)) 
                                .size(Size::remainder())  
                                .vertical(|mut left_strip| {
                                    left_strip.cell(|ui| self.draw_control_panel(ui, &pal));
                                    left_strip.cell(|ui| self.draw_queries_panel(ui, &pal));
                                });
                        });

                        // COLUMNA DERECHA
                        main_strip.cell(|ui| {
                            StripBuilder::new(ui)
                                .size(Size::remainder())  
                                .size(Size::initial(300.0).at_least(200.0)) 
                                .vertical(|mut right_strip| {
                                    right_strip.cell(|ui| self.draw_results_table(ui, &pal));
                                    right_strip.cell(|ui| self.draw_document_visualizer(ui, &pal));
                                });
                        });
                    });
            });
    }
}
