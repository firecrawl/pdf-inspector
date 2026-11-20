//! LaTeX for formula regions read from the PDF's own text layer.
//!
//! A formula a layout model boxes on a born-digital page is usually set
//! from TeX's math fonts, whose glyphs carry what a formula reader needs:
//! characters, sizes and baselines. [`extract_math_latex_in_regions_mem`]
//! reads the glyphs inside each region and rebuilds one line of LaTeX from
//! them — sub- and superscripts from baselines and sizes, limits of large
//! operators, accents, and delimiters built from pieces. Fractions,
//! radicals, matrices and multi-line displays are not rebuilt.
//!
//! Each region also gets a [`MathGate`] saying whether its text layer can
//! be trusted for this at all: a scanned page's OCR layer is invisible or
//! set in a text face, and its LaTeX would only restate the OCR text.

mod glyph;
mod latex;

use crate::extractor::math_glyphs::{MathGlyph, ReadingFrame};
use crate::extractor::{self, PageBox, PositionFrame};
use crate::tounicode::FontCMaps;
use crate::{load_document_from_mem, validate_pdf_bytes, PdfError};
use glyph::{font_key, from_math_glyph, is_math_font, G};
use std::collections::{HashMap, HashSet};

/// Whether a region's text layer is fit to read a formula from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MathGate {
    /// The region's glyphs are visible, decodable, and mostly set in math
    /// fonts: `latex` holds the formula.
    Pass,
    /// No glyph is painted in the region.
    NoGlyphs,
    /// The region holds only invisible text (render mode 3 or 7), the OCR
    /// layer of a scanned page.
    Invisible,
    /// Over a tenth of the region's glyphs decode to nothing usable.
    Unmapped,
    /// Under 60% of the region's glyphs come from math fonts, and its page
    /// paints next to no glyphs in math fonts either: a page set in one
    /// text face, as an OCR engine writes its text layer.
    NonMathFonts,
}

impl MathGate {
    pub fn as_str(&self) -> &'static str {
        match self {
            MathGate::Pass => "pass",
            MathGate::NoGlyphs => "no_glyphs",
            MathGate::Invisible => "invisible",
            MathGate::Unmapped => "unmapped",
            MathGate::NonMathFonts => "non_math_fonts",
        }
    }
}

/// The LaTeX read for one region.
#[derive(Debug, Clone, PartialEq)]
pub struct MathRegionLatex {
    /// One line of LaTeX, without delimiters; empty unless `gate` is
    /// [`MathGate::Pass`].
    pub latex: String,
    pub gate: MathGate,
    /// Visible glyphs whose centre falls in the region.
    pub glyphs: usize,
}

/// The regions of one page, parallel to the rects asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct PageMathRegions {
    /// 0-based page index, as given.
    pub page: u32,
    pub regions: Vec<MathRegionLatex>,
}

const MIN_MATH_FONT_SHARE: f32 = 0.6;
const MAX_UNMAPPED_SHARE: f32 = 0.1;
const MIN_PAGE_MATH_GLYPHS: usize = 3;

/// Read each region of `page_regions` (0-based page, `[x1, y1, x2, y2]`
/// rects in PDF points, top-left origin) as a formula and write it as LaTeX.
/// `frame` says how the rects are read, as for
/// [`crate::extract_text_in_regions_mem_in_frame`]: [`PositionFrame::Sheet`]
/// on the visible page box as laid out (a page whose text runs up or down
/// the sheet is not turned in this frame), [`PositionFrame::Display`] on
/// the rendered page with `/Rotate` applied. Glyphs are read along the
/// page's dominant baseline direction, so formulas on a page whose text
/// runs up the sheet read as they do on an upright one. A glyph belongs to
/// a region when its centre lies inside it, with 1pt of slack along the
/// baseline and 3pt across it.
/// Pages out of range give regions with [`MathGate::NoGlyphs`].
pub fn extract_math_latex_in_regions_mem(
    buffer: &[u8],
    page_regions: &[(u32, Vec<[f32; 4]>)],
    frame: PositionFrame,
) -> Result<Vec<PageMathRegions>, PdfError> {
    validate_pdf_bytes(buffer)?;
    let (doc, _) = load_document_from_mem(buffer)?;
    let pages = doc.get_pages();
    let needed: HashSet<u32> = page_regions
        .iter()
        .filter_map(|(p, _)| p.checked_add(1))
        .collect();
    let font_cmaps = FontCMaps::from_doc_pages_fast(&doc, Some(&needed));
    let mut style_cache = extractor::FontStyleCache::new();
    let mut page_glyphs: HashMap<u32, PageGlyphs> = HashMap::new();
    for page_num in &needed {
        let Some(&page_id) = pages.get(page_num) else {
            continue;
        };
        let page_box = extractor::visible_page_box(&doc, page_id).unwrap_or(PageBox::LETTER);
        let (glyphs, reading) = extractor::math_glyphs::page_math_glyphs(
            &doc,
            page_id,
            page_box,
            &font_cmaps,
            &mut style_cache,
        );
        let math_glyphs = glyphs
            .iter()
            .filter(|m| !m.invisible && is_math_font(&font_key(&m.font)))
            .count();
        page_glyphs.insert(
            *page_num,
            PageGlyphs {
                glyphs,
                reading,
                display: (frame == PositionFrame::Display)
                    .then(|| extractor::DisplayPage::new(&doc, page_id, page_box)),
                math_page: math_glyphs >= MIN_PAGE_MATH_GLYPHS,
            },
        );
    }
    Ok(page_regions
        .iter()
        .map(|(page, rects)| {
            let page_glyphs = page.checked_add(1).and_then(|p| page_glyphs.get(&p));
            let regions = rects
                .iter()
                .map(|rect| match page_glyphs {
                    Some(pg) => {
                        let sheet = match pg.display {
                            Some(d) => d.region_to_sheet(*rect),
                            None => *rect,
                        };
                        region_latex(&pg.glyphs, pg.reading.sheet_rect(sheet), pg.math_page)
                    }
                    None => MathRegionLatex {
                        latex: String::new(),
                        gate: MathGate::NoGlyphs,
                        glyphs: 0,
                    },
                })
                .collect();
            PageMathRegions {
                page: *page,
                regions,
            }
        })
        .collect())
}

struct PageGlyphs {
    glyphs: Vec<MathGlyph>,
    reading: ReadingFrame,
    display: Option<extractor::DisplayPage>,
    math_page: bool,
}

fn region_latex(glyphs: &[MathGlyph], rect: [f32; 4], math_page: bool) -> MathRegionLatex {
    let [ax, ay, bx, by] = rect;
    let (x1, x2, y1, y2) = (ax.min(bx), ax.max(bx), ay.min(by), ay.max(by));
    let mut gs: Vec<G> = Vec::new();
    let (mut invisible, mut unmapped, mut math) = (0usize, 0usize, 0usize);
    for m in glyphs {
        let (g, bad) = from_math_glyph(m);
        let (xc, yc) = match &g {
            Some(g) => (g.xc(), g.yc()),
            None => ((m.x0 + m.x1) / 2.0, m.base - 0.25 * m.size),
        };
        if !(x1 - 1.0 <= xc && xc <= x2 + 1.0 && y1 - 3.0 <= yc && yc <= y2 + 3.0) {
            continue;
        }
        if m.invisible {
            invisible += usize::from(g.is_some() || bad);
            continue;
        }
        if bad {
            unmapped += 1;
        }
        if let Some(g) = g {
            math += usize::from(is_math_font(&font_key(&m.font)));
            gs.push(g);
        }
    }
    let visible = gs.len() + unmapped;
    let gate = if visible == 0 {
        if invisible > 0 {
            MathGate::Invisible
        } else {
            MathGate::NoGlyphs
        }
    } else if unmapped as f32 > MAX_UNMAPPED_SHARE * visible as f32 {
        MathGate::Unmapped
    } else if !math_page && (math as f32) < MIN_MATH_FONT_SHARE * gs.len() as f32 {
        MathGate::NonMathFonts
    } else {
        MathGate::Pass
    };
    let glyph_count = visible;
    let latex = if gate == MathGate::Pass {
        latex::render_line(latex::merge_pieces(gs))
            .trim()
            .to_string()
    } else {
        String::new()
    };
    MathRegionLatex {
        latex,
        gate,
        glyphs: glyph_count,
    }
}
