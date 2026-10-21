//! The fonts of the optional craft-fonts build input (`CRAFT_FONTS_DIR`, see `build.rs`).
//!
//! They sit at the end of every font chain, after the system faces: a machine's own fonts keep
//! deciding how text looks, and the embedded faces only catch the scripts nothing else covers
//! (Japanese, Chinese, Korean, Thai, Hebrew… on a machine without such fonts). Built without
//! craft-fonts, [`CRAFT_FONTS`] is empty and nothing changes.

use std::sync::Arc;

use egui::{FontData, FontDefinitions};

include!(concat!(env!("OUT_DIR"), "/craft_fonts.rs"));

/// The egui font-data key of an embedded face.
fn key(font: &CraftFont) -> String {
    format!("craft-{}-{}", font.family, font.style)
}

/// Adds the embedded faces to `fonts` and returns their keys for interface text and for
/// monospace text, each in the manifest's fallback order. Empty without craft-fonts.
pub fn load(fonts: &mut FontDefinitions) -> (Vec<String>, Vec<String>) {
    let mut chain = |role: CraftFontRole| -> Vec<String> {
        let mut keys = Vec::new();
        for font in craft_font_order(role) {
            let k = key(font);
            fonts.font_data.entry(k.clone()).or_insert_with(|| Arc::new(FontData::from_static(font.bytes)));
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
        keys
    };
    let ui = chain(CraftFontRole::Ui);
    let mono = chain(CraftFontRole::Mono);
    (ui, mono)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_embedded_face_is_loaded_once_and_chained() {
        let mut fonts = FontDefinitions::default();
        let before = fonts.font_data.len();
        let (ui, mono) = load(&mut fonts);
        for k in ui.iter().chain(&mono) {
            assert!(fonts.font_data.contains_key(k), "{k} has data");
        }
        let distinct: std::collections::HashSet<&String> = ui.iter().chain(&mono).collect();
        assert_eq!(fonts.font_data.len(), before + distinct.len(), "one entry per face");
        if CRAFT_FONTS.is_empty() {
            assert!(ui.is_empty() && mono.is_empty(), "nothing without craft-fonts");
        }
    }
}
