use eframe::egui;

#[derive(PartialEq, Clone, Copy)]
pub enum ThemeMode {
    System,
    Light,
    Dark,
}

pub struct Palette {
    pub base_bg: egui::Color32,
    pub panel_bg: egui::Color32,
    pub input_bg: egui::Color32,
    pub primary_text: egui::Color32,
    pub strong_text: egui::Color32,
    pub subdued_text: egui::Color32,
    pub idle_ctrl: egui::Color32,
    pub hover_ctrl: egui::Color32,
    pub active_ctrl: egui::Color32,
    pub row_alt: egui::Color32,
    pub row_hover: egui::Color32,
    pub row_sel: egui::Color32,
    pub match_bg: egui::Color32,
    pub match_fg: egui::Color32,
}

fn hex_color(hex: &str) -> egui::Color32 {
    let hex = hex.trim_start_matches('#');
    if hex.len() == 6 {
        let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
        let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
        let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
        egui::Color32::from_rgb(r, g, b)
    } else {
        egui::Color32::WHITE
    }
}

impl Palette {
    pub fn current(is_dark: bool) -> Self {
        if is_dark {
            Self {
                base_bg: hex_color("#1A1A1D"),
                panel_bg: hex_color("#242427"),
                input_bg: hex_color("#2C2C30"),
                primary_text: hex_color("#E0E0E0"),
                strong_text: hex_color("#E0E0E0"),
                subdued_text: hex_color("#8E8E93"),
                idle_ctrl: hex_color("#0082BD"),
                hover_ctrl: hex_color("#009EE3"),
                active_ctrl: hex_color("#006796"),
                row_alt: hex_color("#1E1E22"),
                row_hover: hex_color("#2A3A45"),
                row_sel: hex_color("#18455E"),
                match_bg: hex_color("#fac3a9"),
                match_fg: hex_color("#111827"),
            }
        } else {
            Self {
                base_bg: hex_color("#e6e8ec"),
                panel_bg: hex_color("#E5E7EB"),
                input_bg: hex_color("#D1D5DB"),
                primary_text: hex_color("#374151"),
                strong_text: hex_color("#111827"),
                subdued_text: hex_color("#6B7280"),
                idle_ctrl: hex_color("#009EE3"),
                hover_ctrl: hex_color("#33B2ED"),
                active_ctrl: hex_color("#007BB0"),
                row_alt: hex_color("#F3F4F6"),
                row_hover: hex_color("#E6F5FC"),
                row_sel: hex_color("#CCEAF7"),
                match_bg: hex_color("#fac3a9"),
                match_fg: hex_color("#111827"),
            }
        }
    }
}
