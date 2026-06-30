use std::path::Path;
//use std::sync::{Arc, Mutex};
//use std::ops::ControlFlow;
use pdf_oxide::document::PdfDocument;
use pdf_oxide::pipeline::{TextPipeline, TextPipelineConfig, ReadingOrderContext};
use pdf_oxide::pipeline::converters::{MarkdownOutputConverter, OutputConverter};
use pdf_oxide::converters::ConversionOptions;
use undoc::{docx::DocxParser, Block};
use crate::models::{TextCandidate, StructuralLocation, CachedDocument, CachedParagraph};
use crate::search::StructuralSearchEngine;

/// Parses an individual line to determine if it contains a Markdown heading.
/// If matched, returns the raw, trimmed inner text, a boolean flag, and the heading level.
fn parse_markdown_heading(line: &str) -> (String, bool, Option<usize>) {
    if line.starts_with('#') {
        let mut chars = line.chars().peekable();
        let mut level = 0;
        while let Some(&'#') = chars.peek() {
            level += 1;
            chars.next();
        }
        if let Some(&' ') = chars.peek() {
            chars.next(); // Consume the delimiter space
            let text: String = chars.collect();
            return (text.trim().to_string(), true, Some(level));
        }
    }
    (line.to_string(), false, None)
}

pub struct DocumentExtractor;

impl DocumentExtractor {
    /// Extract text candidates from a PDF document in true semantic reading order.
    pub fn extract_pdf_stream(&self, path: &Path) -> anyhow::Result<Vec<TextCandidate>> {
        let doc = PdfDocument::open(path)
            .map_err(|e| anyhow::anyhow!("Failed to initialize PdfDocument via pdf_oxide: {:?}", e))?;
        
        let total_pages = doc.page_count()
            .map_err(|e| anyhow::anyhow!("Failed to retrieve page count: {:?}", e))?;
        
        let mut candidates = Vec::new();

        for page_idx in 0..total_pages {
            let spans = doc.extract_spans(page_idx)
                .map_err(|e| anyhow::anyhow!("Failed to extract page spans at page {}: {:?}", page_idx, e))?;
            
            let mut conversion_opts = ConversionOptions::default();
            conversion_opts.detect_headings = true;
            
            let pipeline_config = TextPipelineConfig::from_conversion_options(&conversion_opts);
            let pipeline = TextPipeline::with_config(pipeline_config.clone());
            
            let context = ReadingOrderContext::new();
            let ordered_spans = pipeline.process(spans, context)
                .map_err(|e| anyhow::anyhow!("Failed to process reading order pipeline on page {}: {:?}", page_idx, e))?;
            
            let converter = MarkdownOutputConverter::new();
            let markdown = converter.convert(&ordered_spans, &pipeline_config)
                .map_err(|e| anyhow::anyhow!("Failed to convert ordered spans to Markdown on page {}: {:?}", page_idx, e))?;
            
            let mut block_index = 0;
            for line in markdown.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                let (clean_text, _is_heading, _heading_level) = parse_markdown_heading(trimmed);
                let (normalized_text, mapping) = StructuralSearchEngine::normalize_text_with_mapping(&clean_text);

                candidates.push(TextCandidate {
                    text: clean_text,
                    normalized_text,
                    mapping,
                    location: StructuralLocation::Pdf {
                        page_number: (page_idx + 1) as u32,
                        block_index,
                    },
                });

                block_index += 1;
            }
        }

        Ok(candidates)
    }

    /// KEEP EXACTLY AS IS (DO NOT ALTER)
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
                if trimmed.is_empty() { continue; }

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
                let doc = PdfDocument::open(path)
                    .map_err(|e| anyhow::anyhow!("Failed to open PDF document: {:?}", e))?;
                
                let total_pages = doc.page_count()
                    .map_err(|e| anyhow::anyhow!("Failed to extract page count: {:?}", e))?;
                
                let mut pages = Vec::new();

                for page_idx in 0..total_pages {
                    let spans = doc.extract_spans(page_idx)
                        .map_err(|e| anyhow::anyhow!("Failed to extract page spans at page {}: {:?}", page_idx, e))?;
                    
                    let mut conversion_opts = ConversionOptions::default();
                    conversion_opts.detect_headings = true;
                    
                    let pipeline_config = TextPipelineConfig::from_conversion_options(&conversion_opts);
                    let pipeline = TextPipeline::with_config(pipeline_config.clone());
                    
                    let context = ReadingOrderContext::new();
                    let ordered_spans = pipeline.process(spans, context)
                        .map_err(|e| anyhow::anyhow!("Failed to resolve reading order at page {}: {:?}", page_idx, e))?;
                    
                    let converter = MarkdownOutputConverter::new();
                    let markdown = converter.convert(&ordered_spans, &pipeline_config)
                        .map_err(|e| anyhow::anyhow!("Failed to serialize page output to Markdown at page {}: {:?}", page_idx, e))?;
                    
                    let mut page_paragraphs = Vec::new();
                    let mut original_index = 0;

                    for line in markdown.lines() {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }

                        let (clean_text, is_heading, heading_level) = parse_markdown_heading(trimmed);

                        page_paragraphs.push(CachedParagraph {
                            text: clean_text,
                            original_index,
                            is_heading,
                            heading_level,
                        });

                        original_index += 1;
                    }

                    pages.push(page_paragraphs);
                }

                Ok(CachedDocument::Pdf { pages })
            }
            "docx" => {
                // [KEEP THIS ARM EXACTLY AS IS]
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
                        if trimmed.is_empty() { continue; }

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
