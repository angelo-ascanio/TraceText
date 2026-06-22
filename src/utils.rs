use anyhow::Result;
use compact_str::CompactString;
use rust_xlsxwriter::Workbook;
use std::path::Path;
use crate::app::DisplayRow;

pub fn format_clipboard_tsv(results: &[DisplayRow]) -> String {
    let mut tsv = String::from("Consulta\tPuntuación\tTexto Original Coincidente\tUbicación\n");
    for r in results {
        tsv.push_str(&format!("{}\t{:.2}\t{}\t{}\n", r.query, r.score, r.full_text(), r.location));
    }
    tsv
}

pub fn export_to_excel(results: &[DisplayRow], path: &Path) -> Result<()> {
    let mut workbook = Workbook::new();
    let worksheet = workbook.add_worksheet();

    worksheet.write_string(0, 0, "Consulta")?;
    worksheet.write_string(0, 1, "Puntuación")?;
    worksheet.write_string(0, 2, "Texto Original Coincidente")?;
    worksheet.write_string(0, 3, "Ubicación")?;

    for (row_idx, row) in results.iter().enumerate() {
        let r = (row_idx + 1) as u32;
        worksheet.write_string(r, 0, &row.query)?;
        worksheet.write_number(r, 1, row.score as f64)?;
        worksheet.write_string(r, 2, &row.full_text())?;
        worksheet.write_string(r, 3, &row.location)?;
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