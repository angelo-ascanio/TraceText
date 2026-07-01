use anyhow::Result;
use compact_str::CompactString;
use rust_xlsxwriter::Workbook;
use std::path::Path;
use crate::app::DisplayRow;
use crate::models::{BBox, SpatialIndex};

pub fn format_clipboard_tsv(results: &[DisplayRow]) -> String {
    let mut tsv = String::from("Consulta\tPuntuación\tTexto Original Coincidente\tPágina\n");
    for r in results {
        tsv.push_str(&format!("{}\t{:.2}\t{}\t{}\n", r.query, r.score, r.full_text(), r.page_number));
    }
    tsv
}

pub fn export_to_excel(results: &[DisplayRow], path: &Path) -> Result<()> {
    let mut workbook = Workbook::new();
    let worksheet = workbook.add_worksheet();

    worksheet.write_string(0, 0, "Consulta")?;
    worksheet.write_string(0, 1, "Puntuación")?;
    worksheet.write_string(0, 2, "Texto Original Coincidente")?;
    worksheet.write_string(0, 3, "Página")?;

    for (row_idx, row) in results.iter().enumerate() {
        let r = (row_idx + 1) as u32;
        worksheet.write_string(r, 0, &row.query)?;
        worksheet.write_number(r, 1, row.score as f64)?;
        worksheet.write_string(r, 2, &row.full_text())?;
        worksheet.write_string(r, 3, &row.page_number.to_string())?;
    }

    workbook.save(path)?;
    Ok(())
}

pub fn resolve_highlight(
    raw_text: &str, 
    indices: &[u32]
) -> (CompactString, CompactString, CompactString) {
    if indices.is_empty() {
        return (
            CompactString::from(raw_text),
            CompactString::default(),
            CompactString::default(),
        );
    }

    let start_char_idx = *indices.iter().min().unwrap() as usize;
    let end_char_idx = *indices.iter().max().unwrap() as usize;

    let mut start_byte = 0;
    let mut end_byte = raw_text.len();
    let mut current_char_idx = 0;

    for (byte_idx, c) in raw_text.char_indices() {
        if current_char_idx == start_char_idx {
            start_byte = byte_idx;
        }
        if current_char_idx == end_char_idx {
            end_byte = byte_idx + c.len_utf8();
            break; 
        }
        current_char_idx += 1;
    }

    let prefix = CompactString::from(&raw_text[..start_byte]);
    let match_text = CompactString::from(&raw_text[start_byte..end_byte]);
    let suffix = CompactString::from(&raw_text[end_byte..]);

    (prefix, match_text, suffix)
}

/// Resolves raw character indices into grouped physical bounding boxes.
pub fn resolve_spatial_highlights(
    matched_indices: &[u32],
    spatial_index: &SpatialIndex,
) -> (usize, Vec<BBox>) {
    if matched_indices.is_empty() {
        return (1, vec![]);
    }

    let mut target_page = 1;
    let mut grouped_boxes: Vec<BBox> = Vec::new();
    let mut current_box: Option<BBox> = None;

    let line_tolerance = 3.0; // Typographic points tolerance for vertical line drifting

    for &idx in matched_indices {
        if let Some(coord) = spatial_index.char_mappings.get(idx as usize) {
            if current_box.is_none() {
                target_page = coord.page_number;
            }

            match current_box.as_mut() {
                // If on the same page and on the same horizontal line, expand the box
                Some(b) if target_page == coord.page_number 
                        && (b.bottom - coord.bounds.bottom).abs() < line_tolerance => {
                    b.left = b.left.min(coord.bounds.left);
                    b.right = b.right.max(coord.bounds.right);
                    b.top = b.top.max(coord.bounds.top);
                    b.bottom = b.bottom.min(coord.bounds.bottom);
                }
                // Otherwise, push the completed box and start a new one (e.g., line break)
                _ => {
                    if let Some(b) = current_box.take() {
                        grouped_boxes.push(b);
                    }
                    current_box = Some(coord.bounds.clone());
                    target_page = coord.page_number;
                }
            }
        }
    }

    if let Some(b) = current_box {
        grouped_boxes.push(b);
    }

    (target_page, grouped_boxes)
}

pub fn apply_buffer(text: &str, buffer_size: usize, is_prefix: bool) -> CompactString {
    let char_count = text.chars().count();
    if char_count <= buffer_size {
        return CompactString::from(text);
    }
    
    if is_prefix {
        let skip = char_count - buffer_size;
        let truncated: String = text.chars().skip(skip).collect();
        CompactString::from(format!("...{}", truncated))
    } else {
        let truncated: String = text.chars().take(buffer_size).collect();
        CompactString::from(format!("{}...", truncated))
    }
}