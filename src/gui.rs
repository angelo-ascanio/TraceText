use eframe::egui;
use egui_extras::{Column, TableBuilder, StripBuilder, Size};
use std::{collections::HashMap,path::PathBuf,sync::{mpsc::{channel, Receiver, Sender},Arc, RwLock,},};
use crate::app::DisplayRow;
use crate::extractor::parse_document_by_type;
use crate::models::{CachedDocument, CachedParagraph, ParserRequest, ParserResponse, StructuralLocation,};

/// Componente de control principal de la interfaz de usuario para la aplicación TraceText.
/// Coordina la presentación, captura de interacciones, renderizado continuo y la comunicación asíncrona.
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
    /// Inicializa la estructura del estado de la GUI, configura los canales de mensajería asíncronos
    /// y levanta el hilo secundario dedicado al análisis no bloqueante de documentos estructurados.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let doc_cache = Arc::new(RwLock::new(HashMap::new()));
        
        let (tx_request, rx_request) = channel::<ParserRequest>();
        let (tx_response, rx_response) = channel::<ParserResponse>();
        
        let doc_cache_clone = Arc::clone(&doc_cache);
        let egui_ctx = cc.egui_ctx.clone();

        // Inicialización segura del hilo secundario de procesamiento e ingestión de archivos
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

        // Configuración y aplicación de la paleta de colores corporativos modernos
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = egui::Color32::from_rgb(15, 23, 42); // Slate 900
        visuals.window_fill = egui::Color32::from_rgb(30, 41, 59); // Slate 800
        visuals.extreme_bg_color = egui::Color32::from_rgb(9, 13, 22); // Slate 950
        visuals.faint_bg_color = egui::Color32::from_rgb(30, 41, 59); // Slate 800
        
        // Estilo plano y uniforme para widgets y botones corporativos
        visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(30, 41, 59);
        visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(148, 163, 184));
        visuals.widgets.inactive.corner_radius = egui::CornerRadius::ZERO;
        
        visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(51, 65, 85);
        visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(249, 115, 22)); // Orange 500
        visuals.widgets.hovered.corner_radius = egui::CornerRadius::ZERO;
        
        visuals.widgets.active.bg_fill = egui::Color32::from_rgb(234, 88, 12); // Orange 600
        visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
        visuals.widgets.active.corner_radius = egui::CornerRadius::ZERO;

        // Selección de acento vibrante de color naranja para el resaltado interactivo estándar
        visuals.selection.bg_fill = egui::Color32::from_rgb(234, 88, 12);
        visuals.selection.stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(254, 215, 170));

        cc.egui_ctx.set_visuals(visuals);

        Self {
            file_path: None,
            queries_text: "".to_string(),
            threshold: 85.0,
            buffer_size: 100,
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

    /// Controla la lógica de dibujo e interpretación continua para el visor de documentos de la derecha.
    fn draw_context_visualizer(&mut self, ui: &mut egui::Ui) {
        if self.active_visualization.is_none() || self.active_match_text.is_none() {
            ui.centered_and_justified(|ui| {
                ui.label(
                    egui::RichText::new("Seleccione una fila en la tabla para inspeccionar su contexto estructurado.")
                       .color(egui::Color32::from_rgb(148, 163, 184))
                       .size(14.0)
                );
            });
            return;
        }

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
                                
                                Self::render_paragraph(ui, para, is_target, match_text, pending_scroll);
                            }
                        }
                    },
                    CachedDocument::Docx { paragraphs } => {
                        for para in paragraphs {
                            let is_target = match &visual_data.target_location {
                                StructuralLocation::Docx { global_paragraph_index,.. } => {
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

    /// Renderiza un párrafo individual dentro del visor aplicando las transformaciones de estilo
    /// requeridas si se identifica como la sección objetivo que contiene el emparejamiento.
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
        job.break_on_newline = true;
        job.wrap.max_width = ui.available_width();
        
        let normal_format = TextFormat {
            font_id: FontId::proportional(14.0),
            color: ui.visuals().text_color(),
           ..Default::default()
        };

        if para.is_heading {
            let h_size = match para.heading_level {
                Some(lvl) => (18.0 - (lvl as f32) * 1.5).max(14.0),
                None => 16.0,
            };
            let mut heading_format = TextFormat {
                font_id: FontId::proportional(h_size),
                color: Color32::from_rgb(241, 245, 249),
               ..Default::default()
            };
            if is_target {
                heading_format.background = Color32::from_rgb(69, 26, 3); // Amber 950
                heading_format.underline = Stroke::new(2.0, Color32::from_rgb(249, 115, 22)); // Orange 500
                heading_format.color = Color32::WHITE;
            }
            job.append(&para.text, 0.0, heading_format);
        } else if is_target {
            let highlight_format = TextFormat {
                font_id: FontId::proportional(14.0),
                color: Color32::from_rgb(252, 211, 77), // Amber 300
                background: Color32::from_rgb(69, 26, 3), // Amber 950
                underline: Stroke::new(1.5, Color32::from_rgb(249, 115, 22)), // Orange 500
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

        let response = ui.label(job);

        // Activación del auto-scroll sobre la región geográfica de la interfaz gráfica
        if is_target {
            if let Some(_target) = pending_scroll_target.take() {
                response.scroll_to_me(Some(egui::Align::Center));
            }
        }

        ui.add_space(8.0);
    }
}

impl eframe::App for TraceTextGui {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Recepción y procesamiento continuo no bloqueante de respuestas de análisis
        if let Ok(response) = self.rx_response.try_recv() {
            self.pending_scroll_target = Some(response.target_location.clone());
            self.active_visualization = Some(response);
        }

        // Distribución macro: Strip Horizontal para las dos columnas principales
        StripBuilder::new(ui)
            .size(Size::exact(360.0)) // Columna Izquierda: Slim (ancho fijo)
            .size(Size::remainder())  // Columna Derecha: Absorbe el resto del ancho
            .horizontal(|mut main_strip| {
                
                // ==========================================
                // COLUMNA IZQUIERDA
                // ==========================================
                main_strip.cell(|ui| {
                    StripBuilder::new(ui)
                        // Arriba: Input Section (Alto fijo, ajusta a ~250px según tu contenido)
                        .size(Size::exact(250.0)) 
                        // Abajo: Query Section (Absorbe el resto del alto de la ventana)
                        .size(Size::remainder())  
                        .vertical(|mut left_strip| {
                            
                            // --- IZQUIERDA TOP: Controles e Inputs ---
                            left_strip.cell(|ui| {
                                egui::ScrollArea::vertical()
                                    .id_salt("inputs_control_scroll")
                                    .show(ui, |ui| {
                                        ui.heading(
                                            egui::RichText::new("Panel de Control y Configuración")
                                                .color(egui::Color32::from_rgb(241, 245, 249))
                                                .strong()
                                        );
                                        ui.add_space(8.0);

                                        // 1. Origen de Datos
                                        ui.label(egui::RichText::new("ORIGEN DE DATOS").strong().size(10.0).color(egui::Color32::from_rgb(249, 115, 22)));
                                        ui.horizontal_wrapped(|ui| {
                                            let select_btn = ui.add(egui::Button::new("📁 Seleccionar Documento").fill(egui::Color32::from_rgb(30, 41, 59)));
                                            if select_btn.clicked() {
                                                if let Some(path) = rfd::FileDialog::new().add_filter("Documentos", &["pdf", "docx"]).pick_file() {
                                                    self.file_path = Some(path);
                                                    self.status_message = "Archivo importado de manera exitosa.".into();
                                                }
                                            }

                                            if let Some(path) = &self.file_path {
                                                ui.add(egui::Label::new(egui::RichText::new(path.display().to_string()).color(egui::Color32::from_rgb(226, 232, 240)).monospace()).truncate());
                                            } else {
                                                ui.label(egui::RichText::new("Ningún documento seleccionado.").color(egui::Color32::from_rgb(148, 163, 184)).italics());
                                            }
                                        });
                                        ui.add_space(8.0);

                                        // 2. Parámetros
                                        ui.label(egui::RichText::new("PARÁMETROS DE BÚSQUEDA").strong().size(10.0).color(egui::Color32::from_rgb(249, 115, 22)));
                                        //ui.horizontal_wrapped(|ui| {
                                            ui.add(egui::Slider::new(&mut self.threshold, 0.0..=100.0).text("Umbral (%)"));
                                            ui.add_space(4.0);
                                            ui.add(egui::Slider::new(&mut self.buffer_size, 10..=200).text("Búfer"));
                                            ui.add_space(4.0);
                                            ui.add(egui::Slider::new(&mut self.display_limit, 50..=500).text("Límite"));
                                        //});
                                        ui.add_space(8.0);

                                        // 3. Botonera de Herramientas
                                        ui.horizontal_wrapped(|ui| {
                                            let search_btn = ui.add(egui::Button::new(egui::RichText::new("🔍 Ejecutar Búsqueda").color(egui::Color32::WHITE).strong()).fill(egui::Color32::from_rgb(234, 88, 12)));
                                            if search_btn.clicked() {
                                                if let Some(path) = &self.file_path {
                                                    let queries: Vec<compact_str::CompactString> = self.queries_text.lines().filter(|l| !l.trim().is_empty()).map(|l| compact_str::CompactString::from(l.trim())).collect();
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

                                            if ui.add(egui::Button::new("📊 Excel").fill(egui::Color32::from_rgb(30, 41, 59))).clicked() {
                                                if self.results.is_empty() {
                                                    self.status_message = "Error: No hay datos en la tabla.".into();
                                                } else if let Some(save_path) = rfd::FileDialog::new().add_filter("Excel", &["xlsx"]).set_file_name("Resultados.xlsx").save_file() {
                                                    match crate::utils::export_to_excel(&self.results, &save_path) {
                                                        Ok(_) => self.status_message = "Exportación completada.".into(),
                                                        Err(e) => self.status_message = format!("Fallo en exportación: {}", e),
                                                    }
                                                }
                                            }

                                            if ui.add(egui::Button::new("📋 Copiar").fill(egui::Color32::from_rgb(30, 41, 59))).clicked() {
                                                if !self.results.is_empty() {
                                                    ui.ctx().copy_text(crate::utils::format_clipboard_tsv(&self.results));
                                                    self.status_message = "Transferido al portapapeles.".into();
                                                }
                                            }
                                        });
                                        ui.add_space(4.0);

                                        // 4. Estado
                                        ui.horizontal(|ui| {
                                            ui.label(egui::RichText::new("Estado:").size(11.0).color(egui::Color32::from_rgb(148, 163, 184)));
                                            ui.add(egui::Label::new(egui::RichText::new(&self.status_message).color(egui::Color32::from_rgb(226, 232, 240)).size(11.0)).truncate());
                                        });
                                    });
                            });

                            // --- IZQUIERDA BOTTOM: Multiline Query ---
                            left_strip.cell(|ui| {
                                ui.add_space(6.0);
                                ui.label(egui::RichText::new("PANEL DE CONSULTA (UNA POR LÍNEA)").strong().size(10.0).color(egui::Color32::from_rgb(249, 115, 22)));
                                ui.add_space(4.0);
                                
                                // Ocupa dinámicamente todo el espacio vertical sobrante (Size::remainder)
                                ui.add_sized(
                                    ui.available_size(),
                                    egui::TextEdit::multiline(&mut self.queries_text)
                                        .font(egui::TextStyle::Monospace)
                                        .hint_text("Escriba aquí los términos de búsqueda...")
                                );
                            });
                        });
                });

                // ==========================================
                // COLUMNA DERECHA
                // ==========================================
                main_strip.cell(|ui| {
                    StripBuilder::new(ui)
                        // Arriba: Result Table (Absorbe el alto restante en la derecha)
                        .size(Size::remainder())  
                        // Abajo: Visualizador (Alto base de 300px, pero puede estirarse si se requiere config avanzada)
                        .size(Size::initial(300.0).at_least(200.0)) 
                        .vertical(|mut right_strip| {
                            
                            // --- DERECHA TOP: Tabla de Resultados ---
                            right_strip.cell(|ui| {
                                ui.label(egui::RichText::new("TABLA DE RESULTADOS").strong().size(10.0).color(egui::Color32::from_rgb(249, 115, 22)));
                                ui.add_space(4.0);

                                if self.results.is_empty() {
                                    ui.centered_and_justified(|ui| {
                                        ui.label(egui::RichText::new("Sin coincidencias activas. Defina las consultas y ejecute la búsqueda.").color(egui::Color32::from_rgb(148, 163, 184)).italics());
                                    });
                                } else {
                                    TableBuilder::new(ui)
                                        .id_salt("results_grid")
                                        .striped(true)
                                        .resizable(true)
                                        .sense(egui::Sense::click())
                                        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                                        .column(Column::initial(110.0).at_least(80.0).clip(true))
                                        .column(Column::initial(70.0).at_least(60.0).clip(true))
                                        .column(Column::remainder().clip(true).resizable(false)) // Texto encontrado se estira
                                        .column(Column::initial(120.0).at_least(90.0).clip(true))
                                        .header(22.0, |mut header| {
                                            header.col(|ui| { ui.strong("Consulta"); });
                                            header.col(|ui| { ui.strong("Puntuación"); });
                                            header.col(|ui| { ui.strong("Texto Encontrado"); });
                                            header.col(|ui| { ui.strong("Ubicación"); });
                                        })
                                        .body(|mut body| {
                                            for (index, row) in self.results.iter().enumerate() {
                                                let is_selected = Some(index) == self.selected_row_index;
                                                
                                                body.row(26.0, |mut ui_row| {
                                                    ui_row.set_selected(is_selected);
                                                    let mut row_interacted = false;
                                                    
                                                    ui_row.col(|ui| { if ui.add(egui::Label::new(&row.query).sense(egui::Sense::click())).clicked() { row_interacted = true; }});
                                                    ui_row.col(|ui| {
                                                        let is_perfect = row.score >= 99.9;
                                                        let score_color = if is_perfect { egui::Color32::from_rgb(249, 115, 22) } else if row.score >= 80.0 { egui::Color32::from_rgb(245, 158, 11) } else { ui.visuals().text_color() };
                                                        if ui.add(egui::Label::new(egui::RichText::new(format!("{:.2}", row.score)).strong().color(score_color)).sense(egui::Sense::click())).clicked() { row_interacted = true; }
                                                    });
                                                    ui_row.col(|ui| {
                                                        if row.score > 0.0 {
                                                            let mut job = egui::text::LayoutJob::default();
                                                            job.wrap.max_width = ui.available_width();
                                                            let font_id = egui::TextStyle::Body.resolve(ui.style());
                                                            let text_color = ui.visuals().text_color();
                                                            let is_perfect = row.score >= 99.9;
                                                            let match_color = if is_perfect { egui::Color32::from_rgb(249, 115, 22) } else { egui::Color32::from_rgb(245, 158, 11) };

                                                            job.append(&row.prefix, 0.0, egui::TextFormat { font_id: font_id.clone(), color: text_color, ..Default::default() });
                                                            
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
                                }
                            });

                            // --- DERECHA BOTTOM: Visor de Contexto ---
                            right_strip.cell(|ui| {
                                ui.add_space(8.0);
                                ui.label(egui::RichText::new("VISUALIZADOR DE DOCUMENTO COMPLETO").strong().size(10.0).color(egui::Color32::from_rgb(249, 115, 22)));
                                ui.separator();
                                ui.add_space(4.0);
                                self.draw_context_visualizer(ui);
                            });
                        });
                });
            });
    }
}
