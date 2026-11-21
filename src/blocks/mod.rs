//! Typed block output: the document as a list of classified content
//! blocks per page (headings, paragraphs, lists, tables, code,
//! captions) instead of one markdown string. The pipeline is two
//! page-at-a-time passes — an evidence pass that keeps only small
//! per-line statistics, then an emit pass that classifies each page
//! with the document-wide calibration — so peak memory follows one
//! page, never the document. The markdown pipeline is untouched;
//! this is a separate output surface over the same extraction and
//! detection machinery.

mod emit;
mod evidence;

use std::collections::BTreeMap;

use crate::detector;
use crate::structure_tree::StructTree;
use crate::text_quality::{detect_encoding_issues, is_garbage_text};
use crate::tounicode::FontCMaps;
use crate::{
    add_ocr_reason, page_ocr_reasons_vec, PageOcrReasons, PdfError, PdfOptions, PdfType,
    ProcessingTimer, OCR_REASON_SUSPECTED_GARBLED_TEXT,
};
use lopdf::Document;

pub use emit::{PdfBlock, PdfBlockKind, PdfTable, PdfTableCell};

/// Everything a block-mode parse produces. The detection and quality
/// fields carry the same meanings as [`crate::PdfProcessResult`], so a
/// caller routing on `pdf_type` keeps one story across both surfaces;
/// there is no markdown. Scanned, image-based, and failed-Mixed
/// documents return EMPTY blocks with `pdf_type` carrying the refusal
/// semantics — a text-bearing type with empty blocks means an empty
/// document.
#[derive(Debug)]
pub struct PdfBlocksResult {
    pub pdf_type: PdfType,
    pub page_count: u32,
    pub pages_needing_ocr: Vec<u32>,
    pub ocr_reasons_by_page: Vec<PageOcrReasons>,
    pub title: Option<String>,
    pub confidence: f32,
    pub has_encoding_issues: bool,
    pub processing_time_ms: u64,
    pub blocks: Vec<PdfBlock>,
}

pub(crate) fn process_document_blocks(
    doc: Document,
    page_count: u32,
    options: PdfOptions,
    start: ProcessingTimer,
) -> Result<PdfBlocksResult, PdfError> {
    let detection = detector::detect_from_document(&doc, page_count, &options.detection)?;
    let pdf_type = detection.pdf_type;
    let mut pages_needing_ocr = detection.pages_needing_ocr;
    let title = detection.title;
    let confidence = detection.confidence;
    let mut ocr_reasons_by_page = detection.ocr_reasons_by_page;

    if matches!(pdf_type, PdfType::Scanned | PdfType::ImageBased) {
        return Ok(PdfBlocksResult {
            pdf_type,
            page_count,
            pages_needing_ocr,
            ocr_reasons_by_page: page_ocr_reasons_vec(ocr_reasons_by_page),
            title,
            confidence,
            has_encoding_issues: false,
            processing_time_ms: start.elapsed_ms(),
            blocks: Vec::new(),
        });
    }

    let font_cmaps = FontCMaps::from_doc(&doc);
    let (struct_roles, struct_tables) = StructTree::from_doc(&doc)
        .map(|tree| {
            let page_ids = doc.get_pages();
            let roles = tree.mcid_to_roles(&page_ids);
            let tables = tree.extract_tables(&page_ids);
            let roles = if roles.is_empty() { None } else { Some(roles) };
            (roles, tables)
        })
        .unwrap_or((None, Vec::new()));

    // Evidence pass. For Mixed documents whose visible text layer is
    // garbage, retry with invisible (Tr=3) text included — the OCR
    // text layer behind scanned images — rebuilding every accumulator.
    let ocr_flagged: Vec<u32> = pages_needing_ocr.clone();
    let evidence = match evidence::gather(
        &doc,
        &font_cmaps,
        options.page_filter.as_ref(),
        false,
        struct_roles.as_ref(),
        &ocr_flagged,
        pdf_type,
    ) {
        Ok(evidence) => {
            if pdf_type == PdfType::Mixed
                && (is_garbage_text(&evidence.garbage_sample)
                    || evidence.garbage_sample.trim().is_empty())
            {
                match evidence::gather(
                    &doc,
                    &font_cmaps,
                    options.page_filter.as_ref(),
                    true,
                    struct_roles.as_ref(),
                    &ocr_flagged,
                    pdf_type,
                ) {
                    Ok(invisible) => Some((invisible, true)),
                    Err(_) => Some((evidence, false)),
                }
            } else {
                Some((evidence, false))
            }
        }
        Err(error) => {
            if pdf_type == PdfType::Mixed {
                evidence::gather(
                    &doc,
                    &font_cmaps,
                    options.page_filter.as_ref(),
                    true,
                    struct_roles.as_ref(),
                    &ocr_flagged,
                    pdf_type,
                )
                .ok()
                .map(|evidence| (evidence, true))
            } else {
                return Err(error);
            }
        }
    };

    let Some((evidence, include_invisible)) = evidence else {
        // Mixed document whose extraction failed both ways: nothing to
        // emit, the type itself tells the caller to OCR.
        return Ok(PdfBlocksResult {
            pdf_type,
            page_count,
            pages_needing_ocr,
            ocr_reasons_by_page: page_ocr_reasons_vec(ocr_reasons_by_page),
            title,
            confidence,
            has_encoding_issues: false,
            processing_time_ms: start.elapsed_ms(),
            blocks: Vec::new(),
        });
    };

    // Pages whose OCR-flagged text decoded to CID garbage emit nothing;
    // their rows carry the reason instead.
    for &page in &evidence.garbage_ocr_pages {
        add_ocr_reason(
            &mut ocr_reasons_by_page,
            page,
            OCR_REASON_SUSPECTED_GARBLED_TEXT,
        );
    }
    merge_reasons(&mut ocr_reasons_by_page, evidence.quality_reasons.clone());
    let mut quality_ocr_pages = evidence.quality_ocr_pages.clone();
    pages_needing_ocr.append(&mut quality_ocr_pages);
    pages_needing_ocr.sort_unstable();
    pages_needing_ocr.dedup();

    let calibration = evidence.calibrate();

    let blocks = emit::emit_document(
        &doc,
        &font_cmaps,
        options.page_filter.as_ref(),
        include_invisible,
        &calibration,
        struct_roles.as_ref(),
        &struct_tables,
        &options.markdown,
    )?;

    // Whole-document quality rollups over the emitted text, with the
    // same meanings as the markdown surface: a Mixed document whose
    // text layer is garbage is really a scan; a TextBased document
    // whose text is garbage has undecodable fonts and every page needs
    // OCR.
    let body_text = {
        let mut text = String::new();
        for block in &blocks {
            if !block.furniture {
                text.push_str(&block.text);
                text.push('\n');
            }
        }
        text
    };
    let mut has_encoding_issues = !ocr_reasons_by_page.is_empty()
        || evidence.has_encoding_issues
        || detect_encoding_issues(&body_text);

    if pdf_type == PdfType::Mixed && is_garbage_text(&body_text) {
        return Ok(PdfBlocksResult {
            pdf_type: PdfType::Scanned,
            page_count,
            pages_needing_ocr,
            ocr_reasons_by_page: page_ocr_reasons_vec(ocr_reasons_by_page),
            title,
            confidence: 0.95,
            has_encoding_issues: false,
            processing_time_ms: start.elapsed_ms(),
            blocks: Vec::new(),
        });
    }

    let blocks = if pdf_type == PdfType::TextBased && is_garbage_text(&body_text) {
        has_encoding_issues = true;
        pages_needing_ocr = (1..=page_count).collect();
        Vec::new()
    } else {
        blocks
    };

    Ok(PdfBlocksResult {
        pdf_type,
        page_count,
        pages_needing_ocr,
        ocr_reasons_by_page: page_ocr_reasons_vec(ocr_reasons_by_page),
        title,
        confidence,
        has_encoding_issues,
        processing_time_ms: start.elapsed_ms(),
        blocks,
    })
}

fn merge_reasons(into: &mut BTreeMap<u32, Vec<String>>, from: BTreeMap<u32, Vec<String>>) {
    for (page, reasons) in from {
        for reason in reasons {
            add_ocr_reason(into, page, &reason);
        }
    }
}
