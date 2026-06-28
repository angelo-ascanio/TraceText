use eframe::egui;
use egui_extras::{Column, TableBuilder, StripBuilder, Size};
use std::{collections::HashMap, path::PathBuf, sync::{mpsc::{channel, Receiver, Sender}, Arc, RwLock}};
use crate::app::DisplayRow;
use crate::extractor::parse_document_by_type;
use crate::models::{CachedDocument, CachedParagraph, ParserRequest, ParserResponse, StructuralLocation};
use crate::palette::{Palette, ThemeMode};

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
            active_visualization: None,
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
                            self.file_path = Some(path);
                            self.status_message = "Archivo importado de manera exitosa.".into();
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
                                
                                ui_row.col(|ui| { if ui.add(egui::Label::new(&row.query).sense(egui::Sense::click())).clicked() { row_interacted = true; }});
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
                                ui_row.col(|ui| { if ui.add(egui::Label::new(&row.location).sense(egui::Sense::click())).clicked() { row_interacted = true; }});
                                
                                if ui_row.response().clicked() || row_interacted {
                                    self.selected_row_index = Some(index);
                                    if let (Some(path), Some(loc)) = (&self.file_path, &row.raw_location) {
                                        self.active_match_text = Some(row.match_text.to_string());
                                        let cached_doc = { self.doc_cache.read().unwrap().get(path).cloned() };
                                        if let Some(doc) = cached_doc {
                                            self.pending_scroll_target = Some(loc.clone());
                                            self.active_visualization = Some(crate::models::ParserResponse { file_path: path.clone(), document: doc, target_location: loc.clone() });
                                        } else {
                                            let _ = self.tx_request.send(crate::models::ParserRequest { file_path: path.clone(), target_location: loc.clone() });
                                        }
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

        // Si no hay visualización activa o texto coincidente, mostrar estado vacío centralizado
        if self.active_visualization.is_none() || self.active_match_text.is_none() {
            ui.centered_and_justified(|ui| {
                ui.label(
                    egui::RichText::new("Seleccione una fila en la tabla para inspeccionar su contexto estructurado.")
                        .italics()
                        .color(pal.subdued_text)
                        .size(14.0)
                );
            });
            return;
        }

        // Extraer de forma segura las referencias requeridas para el renderizado
        let visual_data = self.active_visualization.as_ref().unwrap();
        let match_text = self.active_match_text.as_ref().unwrap();
        let pending_scroll = &mut self.pending_scroll_target;

        egui::ScrollArea::vertical()
            .id_salt("visualizer_scroll")
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
                                
                                Self::render_paragraph(ui, para, is_target, match_text, pending_scroll, pal);
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
                            
                            Self::render_paragraph(ui, para, is_target, match_text, pending_scroll, pal);
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
        pending_scroll_target: &mut Option<StructuralLocation>,
        pal: &Palette
    ) {
        use egui::text::{LayoutJob, TextFormat};
        use egui::FontId;

        let mut job = LayoutJob::default();
        job.break_on_newline = true;
        job.wrap.max_width = ui.available_width();
        
        let normal_format = TextFormat {
            font_id: FontId::proportional(14.0),
            color: pal.primary_text,
            ..Default::default()
        };

        if para.is_heading {
            let h_size = match para.heading_level {
                Some(lvl) => (18.0 - (lvl as f32) * 1.5).max(14.0),
                None => 16.0,
            };
            let mut heading_format = TextFormat {
                font_id: FontId::proportional(h_size),
                color: pal.strong_text,
                ..Default::default()
            };
            
            if is_target {
                heading_format.background = pal.match_bg;
                heading_format.color = pal.match_fg;
            }
            job.append(&para.text, 0.0, heading_format);
            
        } else if is_target {
            let highlight_format = TextFormat {
                font_id: FontId::proportional(14.0),
                color: pal.match_fg,
                background: pal.match_bg,
                expand_bg: 1.5,
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

        // --- NUEVO CÓDIGO DE SELECCIÓN Y MENÚ CONTEXTUAL ---

        // Crear un identificador estable basado en la dirección de memoria del párrafo
        let ptr_id = para as *const _ as usize;
        let text_edit_id = ui.id().with(ptr_id);
        let cache_id = text_edit_id.with("cache");

        // Texto temporal (simula solo lectura ya que se sobreescribe cada frame y rechaza mutaciones persistentes)
        let mut temp_text = para.text.clone();

        // Layouter personalizado para aplicar el LayoutJob (Rich Text) al TextEdit
        // [MODIFICACIÓN EGUI 0.34+]: '_text' cambia a '&dyn egui::TextBuffer'
        let mut layouter = |ui: &egui::Ui, _text: &dyn egui::TextBuffer, wrap_width: f32| {
            let mut l_job = job.clone();
            l_job.wrap.max_width = wrap_width;
            ui.painter().layout_job(l_job)
        };

        // Dibujar el párrafo como un TextEdit sin marco para permitir selección nativa sin alterar la interfaz
        let output = egui::TextEdit::multiline(&mut temp_text)
            .id(text_edit_id)
            .desired_width(ui.available_width())
            // [MODIFICACIÓN EGUI 0.34+]: .frame() requiere un egui::Frame en lugar de un booleano
            .frame(egui::Frame::NONE)
            .layouter(&mut layouter)
            .show(ui);

        let response = output.response;

        // Detección de clics secundarios para mantener el rango de selección activo
        let is_secondary_down = ui.input(|i| i.pointer.button_down(egui::PointerButton::Secondary));
        let is_secondary_pressed = ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Secondary));
        let has_secondary_interaction = (is_secondary_down || is_secondary_pressed) && response.hovered();

        // Guardar en caché el estado de selección en frames normales
        if let Some(current_range) = output.cursor_range {
            if !has_secondary_interaction {
                ui.ctx().data_mut(|d| d.insert_temp(cache_id, current_range));
            }
        }

        // Restaurar la selección durante el clic derecho
        if has_secondary_interaction {
            if let Some(cached_range) = ui.ctx().data(|d| d.get_temp::<egui::text::CCursorRange>(cache_id)) {
                let mut state = egui::widgets::text_edit::TextEditState::load(ui.ctx(), text_edit_id)
                    .unwrap_or_default();
                state.cursor.set_char_range(Some(cached_range));
                state.store(ui.ctx(), text_edit_id);
            }
        }

        // Lógica de desplazamiento automático
        if is_target {
            if let Some(_target) = pending_scroll_target.take() {
                response.scroll_to_me(Some(egui::Align::Center));
            }
        }

        // Menú contextual con validación de selección
        response.context_menu(|ui| {
            ui.ctx().memory_mut(|mem| mem.request_focus(text_edit_id));
            let cached_range: Option<egui::text::CCursorRange> = ui.ctx().data(|d| d.get_temp(cache_id));
            
            let has_selection = if let Some(range) = cached_range {
                range.primary != range.secondary
            } else {
                false
            };

            if has_selection {
                if ui.button("📋 Copiar").clicked() {
                    if let Some(range) = cached_range {
                        let char_range = range.as_sorted_char_range();
                        
                        // Extracción segura de caracteres Unicode
                        let selected_text: String = para.text
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

            if ui.button("📋 Copiar Todo").clicked() {
                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                    let _ = clipboard.set_text(para.text.clone());
                }
                ui.close();
            }
        });

        ui.add_space(8.0);
    }
}

impl eframe::App for TraceTextGui {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Channel receiver integration loop
        if let Ok(response) = self.rx_response.try_recv() {
            self.pending_scroll_target = Some(response.target_location.clone());
            self.active_visualization = Some(response);
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
