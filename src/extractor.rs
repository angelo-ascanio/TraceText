use std::fs;
use std::path::Path;
use libreoffice_pure::docx_to_pdf_bytes;
use pdfium_render::prelude::*;
use unicode_normalization::UnicodeNormalization;
use crate::models::{BBox, CharCoordinate, SpatialIndex, TextCandidate};
use crate::search::StructuralSearchEngine;

pub struct DocumentExtractor;

impl DocumentExtractor {
    /// Executes the complete ingestion and spatial extraction pipeline.
    /// Returns the structural candidates for the search engine and the spatial index for rendering.
    pub fn extract_unified_pipeline(&self, path: &Path) -> anyhow::Result<(Vec<TextCandidate>, SpatialIndex)> {
        // 1. Ingest document to PDF bytes (handles both native PDFs and DOCX conversions)
        let pdf_bytes = self.ingest_to_pdf_bytes(path)?;

        // 2. Initialize Pdfium for spatial extraction
        let bindings = Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path("./"))
            .or_else(|_| Pdfium::bind_to_system_library())
            .map_err(|e| anyhow::anyhow!("Failed to bind to Pdfium: {:?}", e))?;
        let pdfium = Pdfium::new(bindings);

        // 3. Extract physical coordinates and build the searchable corpus
        let spatial_index = self.build_spatial_index(&pdfium, &pdf_bytes)?;

        // 4. Package the corpus into a TextCandidate for the search engine
        // Since we unified the pipeline, the entire document's text is treated as a continuous block.
        let raw_text = spatial_index.searchable_corpus.clone();
        
        let (normalized_text, mapping) = StructuralSearchEngine::normalize_text_with_mapping(&raw_text);

        let candidate = TextCandidate {
            text: raw_text,
            normalized_text,
            mapping,
            location: path.display().to_string(),
        };

        // Return the candidate for Rayon and the spatial index for the Egui visualizer
        Ok((vec![candidate], spatial_index))
    }

    /// Ingests a document and returns a standardized PDF byte array.
    /// Native PDFs are read directly, while DOCX files are converted in-memory.
    pub fn ingest_to_pdf_bytes(&self, path: &Path) -> anyhow::Result<Vec<u8>> {
        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_lowercase())
            .unwrap_or_default();

        match extension.as_str() {
            "pdf" => {
                // Native PDF: Read bytes directly from disk without modification.
                let bytes = fs::read(path)
                    .map_err(|e| anyhow::anyhow!("Failed to read native PDF file: {:?}", e))?;
                Ok(bytes)
            }
            "docx" => {
                // Converted DOCX: Read raw bytes and convert to PDF in-memory.
                let docx_bytes = fs::read(path)
                    .map_err(|e| anyhow::anyhow!("Failed to read DOCX file: {:?}", e))?;

                // Execute the non-blocking in-memory conversion.
                // Note: libreoffice-pure v0.4.8 does not expose a ConvertOptions struct 
                // for custom font directories in this signature. Fallback fonts must 
                // be available in the host machine's standard OS font paths.
                let pdf_bytes = docx_to_pdf_bytes(&docx_bytes)
                    .map_err(|e| anyhow::anyhow!("Failed to convert DOCX to PDF bytes via libreoffice-pure: {:?}", e))?;
                
                Ok(pdf_bytes)
            }
            _ => Err(anyhow::anyhow!(
                "Unsupported file format: {}. Only PDF and DOCX are supported.",
                extension
            )),
        }
    }

    /// Parses the PDF byte stream using Pdfium to build a 2D physical bounding box mapping.
    pub fn build_spatial_index(&self, pdfium: &Pdfium, pdf_bytes: &[u8]) -> anyhow::Result<SpatialIndex> {
        let document = pdfium.load_pdf_from_byte_slice(pdf_bytes, None)?;
        
        let mut searchable_corpus = String::new();
        let mut char_mappings = Vec::new();

        for (page_idx, page) in document.pages().iter().enumerate() {
            // Note: Newer versions of pdfium-render safely handle text objects with 
            // overlapping bounding boxes during this extraction pass.
            let text = page.text()?;
            
            for char_obj in text.chars().iter() {
                let char_str = char_obj.unicode_string().unwrap_or_default();
                let bounds = char_obj.tight_bounds()?;
                //let bounds = char_obj.loose_bounds()?;
                
                // NFD Normalization for robust searchability
                for c in char_str.nfd() {
                    searchable_corpus.push(c);
                    
                    char_mappings.push(CharCoordinate {
                        page_number: page_idx + 1, // 1-indexed based on previous architecture
                        bounds: BBox {
                            left: bounds.left().value,
                            bottom: bounds.bottom().value,
                            right: bounds.right().value,
                            top: bounds.top().value,
                        },
                    });
                }
            }
        }
        
        Ok(SpatialIndex { 
            searchable_corpus, 
            char_mappings 
        })
    }

}
