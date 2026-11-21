//! The evidence pass: pages extract one at a time and are dropped
//! after contributing small per-line statistics. Everything the emit
//! pass needs document-wide (heading tiers, paragraph threshold,
//! running-furniture keys, folio patterns, struct-tag usage, quality
//! rollups) is computed from these accumulators, so no stage of block
//! production ever holds more than one page's items.

use std::collections::{BTreeMap, HashMap, HashSet};

use once_cell::sync::Lazy;
use regex::Regex;

use lopdf::Document;

use crate::extractor::{extract_page_unit, group_into_lines, FontStyleCache, PageVecSizes};
use crate::markdown::{dominant_font_size, resolve_line_struct_role, struct_role_heading_level};
use crate::structure_tree::StructRole;
use crate::text_quality::{analyze_text_quality, is_cid_garbage};
use crate::tounicode::FontCMaps;
use crate::types::TextLine;
use crate::PdfError;
use crate::PdfType;

/// One line's calibration-relevant statistics: everything the
/// document-wide passes read from a line, and nothing else. The
/// document's full line set at this granularity is a few megabytes
/// where the lines themselves would be gigabytes.
struct LineStat {
    page: u32,
    y: f32,
    first_size: f32,
    first_bold: bool,
    dominant_size: Option<f32>,
    /// Trimmed text has at least one alphabetic char.
    has_alpha: bool,
    /// Trimmed text is non-empty with NO alphabetic chars (digit-only
    /// folios and issue numbers; these never define heading tiers).
    non_alpha_only: bool,
    /// At least three alphabetic characters (the base-size correction
    /// ignores lines below this).
    alpha3: bool,
    words: usize,
    prose_shaped: bool,
}

/// (trimmed text, x half-points, y half-points): identical text at the
/// identical position is the same physical furniture line.
type FurnitureKey = (String, i32, i32);

const RUNNING_FURNITURE_MIN_PAGES: usize = 3;
const RUNNING_FURNITURE_BAND: f32 = 0.2;

/// The document's own vocabulary, the dehyphenation evidence: whole
/// words and hyphenated compounds as they appear away from the line
/// breaks being repaired (a line's own break fragments are excluded
/// so "evi"/"dence" never count as words).
pub(super) struct Vocabulary {
    pub(super) words: HashSet<String>,
    pub(super) hyphenated: HashSet<String>,
}

pub(super) struct Evidence {
    line_stats: Vec<LineStat>,
    furniture_pages_by_key: HashMap<FurnitureKey, HashSet<u32>>,
    /// Edge-band LINE text seen across pages, position-free: running
    /// footers drift a few points when content above them varies, so
    /// repetition of the line's text within the band is its own
    /// evidence alongside the item-position keys.
    furniture_line_pages: HashMap<String, HashSet<u32>>,
    folio_pages_by_pattern: HashMap<String, HashSet<u32>>,
    /// Vertical text extent per page; the edge bands for furniture and
    /// folio matching adapt to real margins.
    page_extents: HashMap<u32, (f32, f32)>,
    struct_level_counts: HashMap<usize, usize>,
    struct_role_lines: usize,
    words: HashSet<String>,
    hyphenated: HashSet<String>,
    /// The previous observed line ended mid-word; the next line's first
    /// token is that word's tail and must not enter the vocabulary.
    prev_line_hyphenated: bool,
    pub(super) garbage_sample: String,
    pub(super) garbage_ocr_pages: HashSet<u32>,
    pub(super) quality_ocr_pages: Vec<u32>,
    pub(super) quality_reasons: BTreeMap<u32, Vec<String>>,
    pub(super) has_encoding_issues: bool,
}

/// The document-wide decisions the emit pass classifies against.
pub(super) struct Calibration {
    pub(super) base_size: f32,
    pub(super) heading_tiers: Vec<f32>,
    pub(super) paragraph_threshold: f32,
    pub(super) furniture_keys: HashSet<FurnitureKey>,
    pub(super) furniture_line_texts: HashSet<String>,
    pub(super) folio_patterns: HashSet<String>,
    pub(super) page_extents: HashMap<u32, (f32, f32)>,
    pub(super) suppressed_struct_levels: HashSet<usize>,
    pub(super) garbage_ocr_pages: HashSet<u32>,
    pub(super) vocabulary: Vocabulary,
}

impl Calibration {
    pub(super) fn furniture_key_of(item: &crate::types::TextItem) -> FurnitureKey {
        (
            item.text.trim().to_string(),
            (item.x * 2.0).round() as i32,
            (item.y * 2.0).round() as i32,
        )
    }

    /// Whether a y sits in the top/bottom edge band of its page's text
    /// extent — the only region where furniture and folios live.
    pub(super) fn in_edge_band(&self, page: u32, y: f32) -> bool {
        let Some(&(min_y, max_y)) = self.page_extents.get(&page) else {
            return false;
        };
        let extent = max_y - min_y;
        if extent <= 0.0 {
            return false;
        }
        let band = extent * RUNNING_FURNITURE_BAND;
        y <= min_y + band || y >= max_y - band
    }
}

/// Digit runs collapse to one marker: "Page 12 of 240" and
/// "Page 13 of 240" share a pattern. Used only for folio-shaped lines
/// (short, digit-bearing); prose keys use [`strip_edge_digits`] so
/// mid-text numbers keep lines distinct.
fn collapse_digit_runs(text: &str) -> String {
    let mut pattern = String::new();
    let mut in_digits = false;
    for c in text.chars() {
        if c.is_ascii_digit() {
            if !in_digits {
                pattern.push('#');
                in_digits = true;
            }
        } else {
            pattern.push(c.to_ascii_lowercase());
            in_digits = false;
        }
    }
    pattern
}

/// The running-line comparison key: whitespace collapsed, leading and
/// trailing digit runs stripped — "City Bulletin 5" and
/// "6 City Bulletin" both key as "city bulletin", while digits inside
/// prose keep lines distinct.
pub(super) fn strip_edge_digits(text: &str) -> String {
    let ws = text.split_whitespace().collect::<Vec<_>>().join(" ");
    ws.trim_start_matches(|c: char| c.is_numeric())
        .trim_start()
        .trim_end_matches(|c: char| c.is_numeric())
        .trim_end()
        .to_lowercase()
}

/// A page-edge line's folio pattern. None = the line does not look
/// like a folio at all (too wordy, or carries no digits).
pub(super) fn folio_pattern(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 24 {
        return None;
    }
    if !trimmed.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    if trimmed.split_whitespace().count() > 4 {
        return None;
    }
    Some(collapse_digit_runs(trimmed))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn gather(
    doc: &Document,
    font_cmaps: &FontCMaps,
    page_filter: Option<&HashSet<u32>>,
    include_invisible: bool,
    struct_roles: Option<&HashMap<u32, HashMap<i64, StructRole>>>,
    ocr_flagged_pages: &[u32],
    pdf_type: PdfType,
) -> Result<Evidence, PdfError> {
    let pages = doc.get_pages();
    let mut style_cache = FontStyleCache::new();
    let mut page_vec_sizes = PageVecSizes::default();
    let ocr_flagged: HashSet<u32> = ocr_flagged_pages.iter().copied().collect();

    let mut evidence = Evidence {
        line_stats: Vec::new(),
        furniture_pages_by_key: HashMap::new(),
        furniture_line_pages: HashMap::new(),
        folio_pages_by_pattern: HashMap::new(),
        page_extents: HashMap::new(),
        struct_level_counts: HashMap::new(),
        struct_role_lines: 0,
        words: HashSet::new(),
        hyphenated: HashSet::new(),
        prev_line_hyphenated: false,
        garbage_sample: String::new(),
        garbage_ocr_pages: HashSet::new(),
        quality_ocr_pages: Vec::new(),
        quality_reasons: BTreeMap::new(),
        has_encoding_issues: false,
    };
    let mut sample_items = 0usize;

    for (page_num, &page_id) in pages.iter() {
        if let Some(filter) = page_filter {
            if !filter.contains(page_num) {
                continue;
            }
        }
        let unit = extract_page_unit(
            doc,
            page_id,
            *page_num,
            font_cmaps,
            include_invisible,
            &mut style_cache,
            &mut page_vec_sizes,
        )?;

        if sample_items < 200 {
            for item in &unit.items {
                evidence.garbage_sample.push_str(&item.text);
                sample_items += 1;
                if sample_items >= 200 {
                    break;
                }
            }
        }

        // A flagged page whose decoded text is CID garbage emits
        // nothing later; only readable pages feed calibration.
        if ocr_flagged.contains(page_num) && pdf_type == PdfType::TextBased {
            let page_text: String = unit.items.iter().map(|i| i.text.as_str()).collect();
            if is_cid_garbage(&page_text) {
                evidence.garbage_ocr_pages.insert(*page_num);
                continue;
            }
        }

        let quality = analyze_text_quality(&unit.items);
        evidence.has_encoding_issues |= quality.has_encoding_issues;
        evidence.quality_ocr_pages.extend(quality.pages_needing_ocr);
        for (page, reasons) in quality.reasons_by_page {
            evidence
                .quality_reasons
                .entry(page)
                .or_default()
                .extend(reasons);
        }

        let mut extent: Option<(f32, f32)> = None;
        for item in &unit.items {
            if item.text.trim().is_empty() {
                continue;
            }
            let entry = extent.get_or_insert((item.y, item.y));
            entry.0 = entry.0.min(item.y);
            entry.1 = entry.1.max(item.y);
        }
        let Some((min_y, max_y)) = extent else {
            continue;
        };
        evidence.page_extents.insert(*page_num, (min_y, max_y));
        let band = (max_y - min_y) * RUNNING_FURNITURE_BAND;
        if max_y - min_y > 0.0 {
            for item in &unit.items {
                if item.text.trim().is_empty() {
                    continue;
                }
                if item.y > min_y + band && item.y < max_y - band {
                    continue;
                }
                evidence
                    .furniture_pages_by_key
                    .entry(Calibration::furniture_key_of(item))
                    .or_default()
                    .insert(*page_num);
            }
        }

        let lines = group_into_lines(unit.items);
        for line in &lines {
            evidence.observe_line(line, min_y, max_y, struct_roles);
        }
    }

    Ok(evidence)
}

impl Evidence {
    fn observe_line(
        &mut self,
        line: &TextLine,
        min_y: f32,
        max_y: f32,
        struct_roles: Option<&HashMap<u32, HashMap<i64, StructRole>>>,
    ) {
        let text = line.text();
        let trimmed = text.trim();
        let first = line.items.first();
        let words: Vec<&str> = trimmed.split_whitespace().collect();
        let prose_words = words
            .iter()
            .filter(|w| {
                w.chars()
                    .find(|c| c.is_alphabetic())
                    .is_some_and(|c| !c.is_uppercase())
            })
            .count();
        let stat = LineStat {
            page: line.page,
            y: line.y,
            first_size: first.map(|i| i.font_size).unwrap_or(0.0),
            first_bold: first.map(|i| i.is_bold).unwrap_or(false),
            dominant_size: dominant_font_size(line),
            has_alpha: trimmed.chars().any(|c| c.is_alphabetic()),
            non_alpha_only: !trimmed.is_empty() && trimmed.chars().all(|c| !c.is_alphabetic()),
            alpha3: trimmed.chars().filter(|c| c.is_alphabetic()).count() >= 3,
            words: words.len(),
            prose_shaped: trimmed.chars().count() >= 30 && prose_words * 2 >= words.len(),
        };

        let extent = max_y - min_y;
        if extent > 0.0 {
            let band = extent * RUNNING_FURNITURE_BAND;
            if !(line.y > min_y + band && line.y < max_y - band) {
                if let Some(pattern) = folio_pattern(trimmed) {
                    self.folio_pages_by_pattern
                        .entry(pattern)
                        .or_default()
                        .insert(line.page);
                }
                if !trimmed.is_empty() && trimmed.chars().count() <= 120 {
                    let key = strip_edge_digits(trimmed);
                    if !key.is_empty() {
                        self.furniture_line_pages
                            .entry(key)
                            .or_default()
                            .insert(line.page);
                    }
                }
            }
        }

        if let Some(roles) = struct_roles {
            if let Some(role) = resolve_line_struct_role(line, roles) {
                self.struct_role_lines += 1;
                if let Some(level) = struct_role_heading_level(&role) {
                    *self.struct_level_counts.entry(level).or_insert(0) += 1;
                }
            }
        }

        static PLAIN_WORD_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\p{L}{3,}").unwrap());
        static HYPHENATED_RE: Lazy<Regex> =
            Lazy::new(|| Regex::new(r"(\p{L}{2,})-(\p{L}{2,})").unwrap());
        let ends_hyphenated = trimmed.ends_with('-')
            && trimmed
                .chars()
                .rev()
                .nth(1)
                .is_some_and(|c| c.is_alphabetic());
        let mut tokens: Vec<&str> = PLAIN_WORD_RE
            .find_iter(trimmed)
            .map(|m| m.as_str())
            .collect();
        if ends_hyphenated {
            tokens.pop();
        }
        if self.prev_line_hyphenated && !tokens.is_empty() {
            tokens.remove(0);
        }
        for token in tokens {
            self.words.insert(token.to_lowercase());
        }
        for caps in HYPHENATED_RE.captures_iter(trimmed) {
            self.hyphenated.insert(format!(
                "{}-{}",
                caps[1].to_lowercase(),
                caps[2].to_lowercase()
            ));
        }
        self.prev_line_hyphenated = ends_hyphenated;

        self.line_stats.push(stat);
    }

    pub(super) fn calibrate(&self) -> Calibration {
        // Base size: each line votes once with its first item's size
        // (equal weight per line keeps captions from skewing the base),
        // ties broken toward the smaller size for determinism.
        let mut size_counts: HashMap<i32, usize> = HashMap::new();
        for stat in &self.line_stats {
            if stat.first_size >= 9.0 {
                *size_counts
                    .entry((stat.first_size * 10.0) as i32)
                    .or_insert(0) += 1;
            }
        }
        let base_size = size_counts
            .iter()
            .max_by(|(size_a, count_a), (size_b, count_b)| {
                count_a.cmp(count_b).then_with(|| size_b.cmp(size_a))
            })
            .map(|(size, _)| *size as f32 / 10.0)
            .unwrap_or(12.0);
        let base_size = self.correct_base_size(base_size);

        // Heading tiers: sizes well above body, digit-only lines
        // excluded; when nothing clears the ratio gate, bold lines
        // modestly above body define the tiers (books set headings
        // barely above 10pt body).
        let mut heading_sizes: Vec<f32> = self
            .line_stats
            .iter()
            .filter(|stat| stat.first_size / base_size >= 1.2 && !stat.non_alpha_only)
            .map(|stat| stat.first_size)
            .collect();
        heading_sizes.sort_by(|a, b| b.total_cmp(a));
        let mut heading_tiers: Vec<f32> = Vec::new();
        for size in heading_sizes {
            if !heading_tiers.iter().any(|&t| (t - size).abs() < 0.5) {
                heading_tiers.push(size);
            }
        }
        if heading_tiers.is_empty() {
            let mut bold_sizes: Vec<f32> = self
                .line_stats
                .iter()
                .filter(|stat| {
                    stat.has_alpha && stat.first_bold && stat.first_size / base_size >= 1.05
                })
                .map(|stat| stat.first_size)
                .collect();
            bold_sizes.sort_by(|a, b| b.total_cmp(a));
            for size in bold_sizes {
                if !heading_tiers.iter().any(|&t| (t - size).abs() < 0.5) {
                    heading_tiers.push(size);
                }
            }
        }
        heading_tiers.truncate(4);

        // Paragraph threshold: the median same-page line gap, floored
        // by the base size.
        let mut gaps: Vec<f32> = Vec::new();
        let mut prev: Option<(u32, f32)> = None;
        for stat in &self.line_stats {
            if let Some((prev_page, py)) = prev {
                if stat.page == prev_page {
                    let gap = py - stat.y;
                    if gap > 0.0 && gap < base_size * 10.0 {
                        gaps.push(gap);
                    }
                }
            }
            prev = Some((stat.page, stat.y));
        }
        let paragraph_threshold = if gaps.len() < 5 {
            base_size * 1.8
        } else {
            gaps.sort_by(|a, b| a.total_cmp(b));
            (gaps[gaps.len() / 2] * 1.3).max(base_size * 1.5)
        };

        let furniture_keys = self
            .furniture_pages_by_key
            .iter()
            .filter(|(_, pages)| pages.len() >= RUNNING_FURNITURE_MIN_PAGES)
            .map(|(key, _)| key.clone())
            .collect();
        // Line-text repetition proves furniture only when it spans a
        // real share of the document: three pages of a four-page memo,
        // but hundreds of a statute book — a phrase recurring on a few
        // pages of a long document is content, never chrome.
        let line_page_floor = RUNNING_FURNITURE_MIN_PAGES.max(self.page_extents.len() * 3 / 10);
        let furniture_line_texts = self
            .furniture_line_pages
            .iter()
            .filter(|(_, pages)| pages.len() >= line_page_floor)
            .map(|(text, _)| text.clone())
            .collect();
        let folio_patterns = self
            .folio_pages_by_pattern
            .iter()
            .filter(|(_, pages)| pages.len() >= RUNNING_FURNITURE_MIN_PAGES)
            .map(|(pattern, _)| pattern.clone())
            .collect();

        // Struct heading levels tagged on so many lines they are body
        // text, not headings.
        let mut suppressed_struct_levels = HashSet::new();
        if self.struct_role_lines >= 20 {
            for (&level, &count) in &self.struct_level_counts {
                if count as f32 / self.struct_role_lines as f32 > 0.15 {
                    suppressed_struct_levels.insert(level);
                }
            }
        }

        Calibration {
            base_size,
            heading_tiers,
            paragraph_threshold,
            furniture_keys,
            furniture_line_texts,
            folio_patterns,
            page_extents: self.page_extents.clone(),
            suppressed_struct_levels,
            garbage_ocr_pages: self.garbage_ocr_pages.clone(),
            vocabulary: Vocabulary {
                words: self.words.clone(),
                hyphenated: self.hyphenated.clone(),
            },
        }
    }

    /// A base size dominated by promoted (larger) lines means the vote
    /// picked captions or footnotes as "body"; when most text lines sit
    /// well above the voted base and run prose-shaped, the dominant
    /// promoted size is the real body size.
    fn correct_base_size(&self, base_size: f32) -> f32 {
        let mut text_lines = 0usize;
        let mut promoted: HashMap<i32, usize> = HashMap::new();
        let mut promoted_wordy: HashMap<i32, usize> = HashMap::new();
        for stat in &self.line_stats {
            if !stat.alpha3 {
                continue;
            }
            let Some(dominant) = stat.dominant_size else {
                continue;
            };
            text_lines += 1;
            if dominant / base_size >= 1.2 {
                let key = (dominant * 10.0).round() as i32;
                *promoted.entry(key).or_insert(0) += 1;
                if stat.words >= 6 || stat.prose_shaped {
                    *promoted_wordy.entry(key).or_insert(0) += 1;
                }
            }
        }
        let promoted_total: usize = promoted.values().sum();
        if text_lines < 8 || promoted_total * 3 < text_lines {
            return base_size;
        }
        let corrected = promoted
            .iter()
            .max_by(|(size_a, count_a), (size_b, count_b)| {
                count_a.cmp(count_b).then_with(|| size_b.cmp(size_a))
            })
            .map(|(size, _)| *size as f32 / 10.0)
            .unwrap_or(base_size);
        // Only adopt the new base when the promoted lines at that size
        // mostly read like body text (long lines); a genuinely
        // heading-dense document keeps its structure.
        let key = (corrected * 10.0).round() as i32;
        let wordy = promoted_wordy.get(&key).copied().unwrap_or(0);
        let at_size = promoted.get(&key).copied().unwrap_or(0);
        if wordy * 2 < at_size {
            return base_size;
        }
        corrected
    }
}
