//! Font installation.
//!
//! * **Inter** (variable, OFL) is embedded once and instantiated per weight through
//!   the `wght` axis — one 880 KB file covers 200/300/400/600/700.
//! * **JetBrains Mono** (OFL) provides the tabular figures (`mono` token).
//! * **CJK** comes from the system at runtime (PingFang SC / Microsoft YaHei /
//!   Noto Sans CJK / WenQuanYi), appended to *every* family so egui falls back to
//!   it automatically. It is not embedded: the files are 20 MB+.

use std::sync::Arc;

use eframe::egui::epaint::text::VariationCoords;
use eframe::egui::{Context, FontData, FontDefinitions, FontFamily, FontTweak};

use super::tokens::*;

/// Inter 4.1 variable, `wght` 100..900, `opsz` 14..32.
pub const INTER: &[u8] = include_bytes!("../../assets/fonts/InterVariable.ttf");
pub const INTER_ITALIC: &[u8] = include_bytes!("../../assets/fonts/InterVariable-Italic.ttf");
pub const JETBRAINS_MONO: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf");

const MONO_NAME: &str = "jetbrains-mono";
const CJK_NAME: &str = F_CJK;

/// A `FontTweak` that pins the variable font's weight axis.
fn inter_tweak(weight: f32) -> FontTweak {
    FontTweak { coords: VariationCoords::new([("wght", weight)]), ..Default::default() }
}

/// Where a CJK font might live, in preference order.
fn cjk_candidates() -> Vec<&'static str> {
    vec![
        // macOS
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        "/System/Library/Fonts/STHeiti Medium.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/Library/Fonts/Arial Unicode.ttf",
        // Windows
        "C:/Windows/Fonts/msyh.ttc",
        "C:/Windows/Fonts/msyh.ttf",
        "C:/Windows/Fonts/simsun.ttc",
        // Linux
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJKsc-Regular.otf",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/usr/share/fonts/truetype/arphic/uming.ttc",
    ]
}

/// Load the first usable CJK font into `defs`; returns its family key.
fn load_cjk(defs: &mut FontDefinitions) -> Option<String> {
    for path in cjk_candidates() {
        let Ok(bytes) = std::fs::read(path) else { continue };
        // A .ttc holds several faces; face 0 is always a regular weight.
        defs.font_data
            .insert(CJK_NAME.to_owned(), Arc::new(FontData::from_owned(bytes)));
        log::info!("CJK fallback font: {path}");
        return Some(CJK_NAME.to_owned());
    }
    log::warn!("no CJK fallback font found; Chinese metadata may render as boxes");
    None
}

/// Build and install the font definitions.
pub fn install(ctx: &Context) {
    let mut defs = FontDefinitions::default();

    // Inter, one entry per weight (the bytes are borrowed, not copied).
    let weights: [(&str, f32); 5] = [
        (F_XLIGHT, 200.0),
        (F_LIGHT, 300.0),
        (F_REG, 400.0),
        (F_SEMI, 600.0),
        (F_BOLD, 700.0),
    ];
    for (name, weight) in weights {
        defs.font_data
            .insert(name.to_owned(), Arc::new(FontData::from_static(INTER).tweak(inter_tweak(weight))));
    }
    for (name, weight) in [(F_LIGHT_ITALIC, 300.0), (F_ITALIC, 400.0)] {
        defs.font_data.insert(
            name.to_owned(),
            Arc::new(FontData::from_static(INTER_ITALIC).tweak(inter_tweak(weight))),
        );
    }
    defs.font_data.insert(
        MONO_NAME.to_owned(),
        Arc::new(FontData::from_static(JETBRAINS_MONO)),
    );

    let cjk = load_cjk(&mut defs);
    // Emoji last: epaint's defaults already ship a colour emoji font and a
    // fallback for rare symbols.
    let mut fallback: Vec<String> = Vec::new();
    if let Some(cjk) = cjk {
        fallback.push(cjk);
    }
    fallback.push("NotoEmoji-Regular".to_owned());
    fallback.push("emoji-icon-font".to_owned());

    let with_fallback = |first: &[&str]| -> Vec<String> {
        first
            .iter()
            .map(|s| (*s).to_owned())
            .chain(fallback.iter().cloned())
            .collect()
    };

    for name in [F_XLIGHT, F_LIGHT, F_REG, F_SEMI, F_BOLD, F_LIGHT_ITALIC, F_ITALIC] {        defs.families
            .insert(FontFamily::Name(name.into()), with_fallback(&[name]));
    }
    defs.families
        .insert(FontFamily::Proportional, with_fallback(&[F_REG]));
    defs.families
        .insert(FontFamily::Monospace, with_fallback(&[MONO_NAME]));

    ctx.set_fonts(defs);
}
