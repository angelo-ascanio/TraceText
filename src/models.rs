use compact_str::CompactString;
use std::path::PathBuf;

// --- Search & Extraction Models ---

#[derive(Debug, Clone, serde::Serialize)]
pub struct QueryMatch {
    pub query: CompactString,
    pub matches: bool,
    pub similarity_score: f32,
    pub prefix: CompactString,
    pub match_text: CompactString,
    pub suffix: CompactString,
    pub matched_indices: Vec<u32>,
}

// #[derive(Debug, Clone, serde::Serialize)]
// #[serde(tag = "type", rename_all = "snake_case")]
// pub enum StructuralLocation {
//     Pdf { 
//         page_number: u32, 
//         block_index: usize,
//     },
//     Docx { 
//         global_paragraph_index: usize, 
//         heading_context: String, 
//     },
// }

#[derive(Debug, Clone)]
pub struct TextCandidate {
    pub text: String,
    pub normalized_text: String,
    pub mapping: Vec<usize>,
    #[allow(dead_code)]
    pub location: String,
}

// --- Visualizer & Caching Models ---

// #[allow(dead_code)]
// #[derive(Debug, Clone)]
// pub struct CachedParagraph {
//     pub text: String,
//     pub original_index: usize,
//     pub is_heading: bool,
//     pub heading_level: Option<usize>,
// }

// #[derive(Debug, Clone)]
// pub enum CachedDocument {
//     Pdf {
//         pages: Vec<Vec<CachedParagraph>>,
//     },
//     Docx {
//         paragraphs: Vec<CachedParagraph>,
//     },
// }




// --- Thread Communication Models ---

/// Represents a physical bounding box extracted from a PDF page (PostScript points).
#[derive(Debug, Clone)]
pub struct BBox {
    pub left: f32,
    pub bottom: f32,
    pub right: f32,
    pub top: f32,
}

/// Correlates every character index in the corpus to its exact physical location.
#[derive(Debug, Clone)]
pub struct CharCoordinate {
    pub page_number: usize,
    pub bounds: BBox,
}

/// The updated indexing structure mapping characters to 2D bounding boxes.
#[derive(Debug, Clone)]
pub struct SpatialIndex {
    /// The flat, searchable text string containing NFD-normalized characters.
    pub searchable_corpus: String,
    pub char_mappings: Vec<CharCoordinate>,
}

// --- IPC Messaging ---

pub enum ParserRequest {
    /// Dispatched when a document is first selected to build the searchable index.
    IngestDocument { file_path: PathBuf },
    /// Dispatched when a user clicks a result to fetch a rasterized page.
    FetchPage {
        file_path: PathBuf,
        page_index: usize,
        target_width_px: i32,
    },
}

#[allow(dead_code)]
pub enum ParserResponse {
    /// Returns the built SpatialIndex for the search engine cache.
    DocumentIndexed { 
        file_path: PathBuf, 
        spatial_index: SpatialIndex 
    },
    /// Returns raw RGBA pixel data and dimensions for egui GPU upload.
    PageImage {
        file_path: PathBuf,
        page_index: usize,
        rgba_buffer: Vec<u8>,
        width_px: usize,
        height_px: usize,
        page_width_points: f32,
        page_height_points: f32,
    },
    Error(String),
}

// pub struct ParserRequest {
//     pub file_path: PathBuf,
//     pub target_location: StructuralLocation,
// }

// #[allow(dead_code)]
// pub struct ParserResponse {
//     pub file_path: PathBuf,
//     pub document_bytes: Vec<u8>,
//     pub target_location: StructuralLocation,
// }