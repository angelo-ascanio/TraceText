use std::path::Path;
use std::sync::{Arc, Mutex};
use std::ops::ControlFlow;
use unpdf::{PdfParser, ParseEvent, PageStreamOptions};
use unpdf::model::Block as PdfBlock; // Aliased to prevent conflict with undoc::Block
use undoc::{docx::DocxParser, Block};
use crate::models::{TextCandidate, StructuralLocation, CachedDocument, CachedParagraph};
use crate::search::StructuralSearchEngine;

pub struct DocumentExtractor;

impl DocumentExtractor {
    /// Extracts text candidates from a PDF document using a streaming parser.
    /// Utilizes unpdf's native structural preservation to read elements in their logical visual order.
    pub fn extract_pdf_stream(&self, path: &Path) -> anyhow::Result<Vec<TextCandidate>> {
        let parser = PdfParser::open(path)?;
        let candidates_accumulator = Arc::new(Mutex::new(Vec::new()));
        let candidates_clone = Arc::clone(&candidates_accumulator);

        parser.for_each_page(PageStreamOptions::default(), move |event| {
            if let ParseEvent::PageParsed(page) = event {
                let mut page_candidates = Vec::new();
                
                // unpdf 0.7.0 inherently preserves structure and reading order,
                // rendering manual spatial/coordinate baseline sorting unnecessary.
                // Fix: Iterating over `elements` instead of `blocks`
                for (sorted_idx, block) in page.elements.iter().enumerate() {
                    let page_text = match block {
                        PdfBlock::Paragraph(para) => para.plain_text(),
                        PdfBlock::Table(table) => table.plain_text(),
                        _ => String::new(),
                    };

                    let trimmed = page_text.trim();
                    if !trimmed.is_empty() {
                        let (normalized, mapping) = StructuralSearchEngine::normalize_text_with_mapping(&page_text);
                        page_candidates.push(TextCandidate {
                            text: page_text,
                            normalized_text: normalized,
                            mapping,
                            location: StructuralLocation::Pdf {
                                page_number: page.number,
                                block_index: sorted_idx,
                            },
                        });
                    }
                }

                if let Ok(mut accumulator) = candidates_clone.lock() {
                    accumulator.extend(page_candidates);
                }
            }
            ControlFlow::Continue(())
        })?;

        let result = Arc::try_unwrap(candidates_accumulator)
           .map_err(|_| anyhow::anyhow!("Arc deallocation failure during PDF candidate collection"))?
           .into_inner()
           .map_err(|_| anyhow::anyhow!("Mutex acquisition failure during PDF candidate collection"))?;

        Ok(result)
    }

    /// Extracts text candidates from a DOCX document sequentially.
    /// Preserves existing architectural bounds and headings context tracking.
    pub fn extract_docx(&self, path: &Path) -> anyhow::Result<Vec<TextCandidate>> {
        let doc = DocxParser::open(path)?.parse()?;
        let mut candidates = Vec::new();
        let mut current_heading = String::new();
        let mut global_para_count = 0;

        for section in &doc.sections {
            for block in &section.content {
                let text = match block {
                    Block::Paragraph(para) => para.plain_text(),
                    Block::Table(table) => table.plain_text(),
                    _ => String::new(),
                };

                let trimmed = text.trim();
                if trimmed.is_empty() {
                    continue;
                }

                // Track heading contexts dynamically based on specific length and punctuation rules.
                if trimmed.len() > 2 && trimmed.len() < 85 && !trimmed.ends_with('.') && !trimmed.ends_with(':') {
                    let char_count = trimmed.chars().count();
                    current_heading = if char_count > 50 {
                        let truncated: String = trimmed.chars().take(50).collect();
                        format!("{}...", truncated)
                    } else {
                        trimmed.to_string()
                    };
                }

                let (normalized, mapping) = StructuralSearchEngine::normalize_text_with_mapping(&text);
                candidates.push(TextCandidate {
                    text: text.clone(),
                    normalized_text: normalized,
                    mapping,
                    location: StructuralLocation::Docx {
                        global_paragraph_index: global_para_count,
                        heading_context: current_heading.clone(),
                    },
                });

                global_para_count += 1;
            }
        }

        Ok(candidates)
    }

    /// Parses documents into a unified cached structural representation.
    pub fn parse_document_by_type(&self, path: &Path) -> anyhow::Result<CachedDocument> {
        let extension = path
           .extension()
           .and_then(|ext| ext.to_str())
           .map(|ext| ext.to_lowercase())
           .unwrap_or_default();

        match extension.as_str() {
            "pdf" => {
                let doc = unpdf::parse_file(path)?;
                let mut pages = Vec::new();

                for page in &doc.pages {
                    let mut cached_paragraphs = Vec::new();
                    
                    // Utilize unpdf's sequential elements
                    // Fix: Iterating over `elements` instead of `blocks`
                    for (sorted_idx, block) in page.elements.iter().enumerate() {
                        let text = match block {
                            PdfBlock::Paragraph(para) => para.plain_text(),
                            PdfBlock::Table(table) => table.plain_text(),
                            _ => String::new(),
                        };

                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            cached_paragraphs.push(CachedParagraph {
                                text,
                                original_index: sorted_idx,
                                is_heading: false,
                                heading_level: None,
                            });
                        }
                    }
                    pages.push(cached_paragraphs);
                }

                Ok(CachedDocument::Pdf { pages })
            }
            "docx" => {
                let doc = DocxParser::open(path)?.parse()?;
                let mut paragraphs = Vec::new();
                let mut global_para_count = 0;

                for section in &doc.sections {
                    for block in &section.content {
                        let text = match block {
                            Block::Paragraph(para) => para.plain_text(),
                            Block::Table(table) => table.plain_text(),
                            _ => String::new(),
                        };

                        let trimmed = text.trim();
                        if trimmed.is_empty() {
                            continue;
                        }

                        paragraphs.push(CachedParagraph {
                            text,
                            original_index: global_para_count,
                            is_heading: false,
                            heading_level: None,
                        });

                        global_para_count += 1;
                    }
                }

                Ok(CachedDocument::Docx { paragraphs })
            }
            _ => Ok(CachedDocument::Docx { paragraphs: Vec::new() }),
        }
    }
}
