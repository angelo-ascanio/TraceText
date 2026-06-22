use compact_str::CompactString;
use std::path::PathBuf;

// --- Search & Extraction Models ---

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

// --- Visualizer & Caching Models ---

#[allow(dead_code)]
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

// --- Thread Communication Models ---

pub struct ParserRequest {
    pub file_path: PathBuf,
    pub target_location: StructuralLocation,
}

#[allow(dead_code)]
pub struct ParserResponse {
    pub file_path: PathBuf,
    pub document: CachedDocument,
    pub target_location: StructuralLocation,
}