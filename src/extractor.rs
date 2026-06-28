use anyhow::{Context, Result};
use std::{ops::ControlFlow, path::Path};
use undoc::docx::DocxParser;
use unpdf::{PageStreamOptions, ParseEvent, PdfParser};
use crate::models::{CachedDocument, CachedParagraph, StructuralLocation, TextCandidate};
use crate::search::StructuralSearchEngine;

/// Reconstructs the logical structure of PDF and DOCX files.
/// Addresses line fragmentation and preserves structural metadata.
pub struct DocumentExtractor;

impl DocumentExtractor {
    /// Reconstructs paragraphs from fragmented PDF text blocks using spatial and lexical heuristics.
    pub fn cluster_pdf_elements(elements: &[unpdf::model::Block]) -> Vec<CachedParagraph> {
        let mut clustered = Vec::new();
        let mut current_paragraph = String::new();
        let mut current_start_idx = 0;
        
        let mut prev_font_size: Option<f32> = None;
        let mut prev_is_table = false;

        for (elem_idx, element) in elements.iter().enumerate() {
            let mut element_text = String::new();
            element.append_plain_text(&mut element_text);
            let trimmed_element = element_text.trim();

            if trimmed_element.is_empty() {
                continue;
            }

            // Extract core font attributes from the underlying text elements
            let mut font_size = 10.0;
            let mut is_table = false;

            match element {
                unpdf::model::Block::Paragraph(para) => {
                    for content in &para.content {
                        if let unpdf::model::InlineContent::Text(run) = content {
                            if let Some(size) = run.style.font_size {
                                font_size = size;
                            }
                            break;
                        }
                    }
                }
                unpdf::model::Block::Table(_) => {
                    is_table = true;
                }
                _ => {}
            }

            if current_paragraph.is_empty() {
                current_paragraph = element_text;
                current_start_idx = elem_idx;
                prev_font_size = Some(font_size);
                prev_is_table = is_table;
            } else {
                let should_merge = {
                    if is_table || prev_is_table {
                        // Prevent merging table cells with standard paragraphs
                        false
                    } else if let Some(prev_size) = prev_font_size {
                        // Spacing Heuristic 1: Validate font size consistency
                        let font_size_match = (font_size - prev_size).abs() < 1.0;

                        // Spacing Heuristic 2: Check for terminal punctuation
                        let trimmed_current = current_paragraph.trim_end();
                        let ends_with_hyphen = trimmed_current.ends_with('-');
                        let ends_with_terminal = trimmed_current.ends_with(|c| {
                            c == '.' || c == '?' || c == '!' || c == ':' || c == '"' || c == ')'
                        });

                        // Spacing Heuristic 3: Check casing on the next element
                        let starts_with_lowercase = trimmed_element
                            .chars()
                            .next()
                            .map_or(false, |c| c.is_lowercase());

                        if ends_with_hyphen {
                            // High probability of a line-wrapped hyphenated word
                            true
                        } else if !ends_with_terminal && font_size_match {
                            // Spatially aligned continuous flow
                            true
                        } else if ends_with_terminal && starts_with_lowercase && font_size_match {
                            // Catch-all for lowercase continuations following punctuation (e.g. abbreviations)
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                };

                if should_merge {
                    let trimmed_current = current_paragraph.trim_end();
                    let trimmed_next = trimmed_element;

                    if trimmed_current.ends_with('-') && trimmed_next.chars().next().map_or(false, |c| c.is_lowercase()) {
                        // Join hyphenated words (e.g. "reconstruct-" and "ion" -> "reconstruction")
                        let stripped = trimmed_current.strip_suffix('-').unwrap_or(trimmed_current);
                        current_paragraph = format!("{}{}", stripped, trimmed_next);
                    } else {
                        // Normalize word spacing during line merging
                        let spacer = if trimmed_current.ends_with(' ') || trimmed_next.starts_with(' ') {
                            ""
                        } else {
                            " "
                        };
                        current_paragraph = format!("{}{}{}", trimmed_current, spacer, trimmed_next);
                    }
                } else {
                    // Flush the current paragraph block to cache
                    clustered.push(CachedParagraph {
                        text: current_paragraph.clone(),
                        original_index: current_start_idx,
                        is_heading: false,
                        heading_level: None,
                    });

                    // Reset parameters for the next block
                    current_paragraph = element_text;
                    current_start_idx = elem_idx;
                    prev_font_size = Some(font_size);
                    prev_is_table = is_table;
                }
            }
        }

        // Flush any remaining text in the paragraph buffer
        if !current_paragraph.is_empty() {
            clustered.push(CachedParagraph {
                text: current_paragraph,
                original_index: current_start_idx,
                is_heading: false,
                heading_level: None,
            });
        }

        // Post-processing pass: Identify logical headings within the clustered blocks
        for para in &mut clustered {
            let trimmed = para.text.trim();
            let count = trimmed.chars().count();

            // Headings are typically short, lack ending punctuation, and fit within a single line
            if count > 2 && count < 80 && !trimmed.ends_with('.') && !trimmed.ends_with(':') {
                para.is_heading = true;
                para.heading_level = Some(1);
            }
        }

        clustered
    }

    /// Recursively processes native OOXML blocks to identify headings, tables, and lists.
    pub fn extract_docx_blocks(block: &undoc::Block, global_para_count: &mut usize) -> Vec<CachedParagraph> {
        let mut extracted = Vec::new();

        match block {
            undoc::Block::Paragraph(para) => {
                *global_para_count += 1;
                let mut text_output = String::new();
                let mut is_heading = false;
                let mut heading_level = None;

                // Query heading levels
                let heading_repr = format!("{:?}", para.heading);
                if !heading_repr.contains("None") {
                    is_heading = true;
                    heading_level = Some(if heading_repr.contains("H2") || heading_repr.contains("Level2") {
                        2
                    } else if heading_repr.contains("H3") || heading_repr.contains("Level3") {
                        3
                    } else if heading_repr.contains("H4") || heading_repr.contains("Level4") {
                        4
                    } else {
                        1
                    });
                }

                // Query and format list structures
                let list_repr = format!("{:?}", para.list_info);
                if !list_repr.contains("None") {
                    let marker = if list_repr.contains("Numbered") || list_repr.contains("Decimal") || list_repr.contains("Ordered") {
                        "1. "
                    } else {
                        "• "
                    };
                    text_output.push_str(marker);
                }

                text_output.push_str(&para.plain_text());

                extracted.push(CachedParagraph {
                    text: text_output,
                    original_index: *global_para_count,
                    is_heading,
                    heading_level,
                });
            }
            undoc::Block::Table(table) => {
                // Parse tables row-by-row and cell-by-cell.
                // Prevents text in separate columns from merging horizontally.
                for row in &table.rows {
                    for cell in &row.cells {
                        let cell_text = cell.plain_text();
                        let trimmed = cell_text.trim();
                        if !trimmed.is_empty() {
                            *global_para_count += 1;
                            extracted.push(CachedParagraph {
                                text: cell_text,
                                original_index: *global_para_count,
                                is_heading: false,
                                heading_level: None,
                            });
                        }
                    }
                }
            }
            _ => {}
        }

        extracted
    }

    /// Reconstructs logical paragraphs from physical streams to produce structured text candidates.
    pub fn extract_pdf_stream<P: AsRef<Path>>(&self, path: P) -> Result<Vec<TextCandidate>> {
        let parser = PdfParser::open(path).context("Failed to initialize unpdf parser")?;
        let accumulator = std::sync::Mutex::new(Vec::new());

        parser.for_each_page(PageStreamOptions::default(), |event| {
            if let ParseEvent::PageParsed(page) = event {
                let clustered = Self::cluster_pdf_elements(&page.elements);
                let mut page_candidates = Vec::new();

                for paragraph in clustered {
                    let raw_text = paragraph.text.clone();
                    // Normalizing after rebuilding the paragraph ensures index mapping alignment
                    let (normalized, mapping) = StructuralSearchEngine::normalize_text_with_mapping(&raw_text);

                    page_candidates.push(TextCandidate {
                        text: raw_text,
                        normalized_text: normalized,
                        mapping,
                        location: StructuralLocation::Pdf {
                            page_number: page.number,
                            block_index: paragraph.original_index,
                        },
                    });
                }

                if !page_candidates.is_empty() {
                    accumulator.lock().unwrap().extend(page_candidates);
                }
            }
            ControlFlow::Continue(())
        }).context("PDF streaming iteration failed")?;

        Ok(accumulator.into_inner().unwrap())
    }

    /// Extracts structural candidates from OOXML DOCX files, preserving headings, lists, and tables.
    pub fn extract_docx<P: AsRef<Path>>(&self, path: P) -> Result<Vec<TextCandidate>> {
        let mut parser = DocxParser::open(path).context("Failed to initialize OOXML DOCX parser")?;
        let doc = parser.parse().context("Failed to parse internal document structures")?;
        let mut candidates = Vec::new();

        let mut current_heading = String::from("Start of Document");
        let mut global_para_count = 0;

        for section in doc.sections.iter() {
            for block in section.content.iter() {
                let paragraphs = Self::extract_docx_blocks(block, &mut global_para_count);

                for paragraph in paragraphs {
                    let text_trimmed = paragraph.text.trim();
                    if text_trimmed.is_empty() {
                        continue;
                    }

                    if paragraph.is_heading {
                        current_heading = text_trimmed.chars().take(50).collect::<String>();
                        if text_trimmed.chars().count() > 50 {
                            current_heading.push_str("...");
                        }
                    }

                    let raw_text = paragraph.text.clone();
                    let (normalized, mapping) = StructuralSearchEngine::normalize_text_with_mapping(&raw_text);

                    candidates.push(TextCandidate {
                        text: raw_text,
                        normalized_text: normalized,
                        mapping,
                        location: StructuralLocation::Docx {
                            global_paragraph_index: paragraph.original_index,
                            heading_context: current_heading.clone(),
                        },
                    });
                }
            }
        }
        Ok(candidates)
    }
}

/// Parses documents and constructs cached models.
/// Shares the same layout algorithms to maintain coordinate alignment.
pub fn parse_document_by_type(path: &Path) -> CachedDocument {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();

    if ext == "pdf" {
        let mut pages = Vec::new();
        if let Ok(parser) = PdfParser::open(path) {
            let _ = parser.for_each_page(PageStreamOptions::default(), |event| {
                if let ParseEvent::PageParsed(page) = event {
                    let clustered = DocumentExtractor::cluster_pdf_elements(&page.elements);
                    pages.push(clustered);
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
                        let extracted = DocumentExtractor::extract_docx_blocks(block, &mut global_para_count);
                        paragraphs.extend(extracted);
                    }
                }
            }
        }
        CachedDocument::Docx { paragraphs }
    } else {
        CachedDocument::Docx { paragraphs: vec![] }
    }
}
