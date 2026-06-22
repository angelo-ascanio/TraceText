use anyhow::{Context, Result};
use std::{ops::ControlFlow, path::Path};
use undoc::docx::DocxParser;
use unpdf::{PageStreamOptions, ParseEvent, PdfParser};
use crate::models::{CachedDocument, CachedParagraph, StructuralLocation, TextCandidate};
use crate::search::StructuralSearchEngine;

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
                        // Relies on the StructuralSearchEngine being moved to search.rs
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

pub fn parse_document_by_type(path: &Path) -> CachedDocument {
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