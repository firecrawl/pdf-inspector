//! Smart PDF detection and text extraction using lopdf
//!
//! This module provides:
//! - Fast detection of scanned vs text-based PDFs without full document load
//! - Direct text extraction from text-based PDFs
//! - Markdown conversion with structure detection

pub mod detector;
pub mod extractor;
pub mod markdown;
pub mod tables;
pub mod tounicode;

pub use detector::{detect_pdf_type, PdfType, PdfTypeResult};
pub use extractor::{extract_text, extract_text_with_positions, TextItem};
pub use markdown::{to_markdown, to_markdown_from_items, MarkdownOptions};

use std::path::Path;

/// High-level PDF processing result
#[derive(Debug)]
pub struct PdfProcessResult {
    /// The detected PDF type
    pub pdf_type: PdfType,
    /// Extracted text (if text-based PDF)
    pub text: Option<String>,
    /// Markdown output (if text-based PDF)
    pub markdown: Option<String>,
    /// Page count
    pub page_count: u32,
    /// Processing time in milliseconds
    pub processing_time_ms: u64,
}

/// Process a PDF file with smart detection and extraction
///
/// This function will:
/// 1. Quickly detect if the PDF is text-based or scanned
/// 2. If text-based, extract text and convert to markdown
/// 3. If scanned, return early indicating OCR is needed
pub fn process_pdf<P: AsRef<Path>>(path: P) -> Result<PdfProcessResult, PdfError> {
    let start = std::time::Instant::now();

    // Step 1: Smart detection (fast, no full load)
    let detection = detect_pdf_type(&path)?;

    let result = match detection.pdf_type {
        PdfType::TextBased => {
            // Step 2: Full extraction with position-aware reading order
            let items = extract_text_with_positions(&path)?;
            let markdown = to_markdown_from_items(items, MarkdownOptions::default());

            PdfProcessResult {
                pdf_type: PdfType::TextBased,
                text: None, // We now produce markdown directly
                markdown: Some(markdown),
                page_count: detection.page_count,
                processing_time_ms: start.elapsed().as_millis() as u64,
            }
        }
        PdfType::Scanned | PdfType::ImageBased => {
            // Return early - OCR needed
            PdfProcessResult {
                pdf_type: detection.pdf_type,
                text: None,
                markdown: None,
                page_count: detection.page_count,
                processing_time_ms: start.elapsed().as_millis() as u64,
            }
        }
        PdfType::Mixed => {
            // Try to extract what we can with position-aware reading order
            let items = extract_text_with_positions(&path).ok();
            let markdown = items.map(|i| to_markdown_from_items(i, MarkdownOptions::default()));

            PdfProcessResult {
                pdf_type: PdfType::Mixed,
                text: None,
                markdown,
                page_count: detection.page_count,
                processing_time_ms: start.elapsed().as_millis() as u64,
            }
        }
    };

    Ok(result)
}

/// Process PDF from memory buffer
pub fn process_pdf_mem(buffer: &[u8]) -> Result<PdfProcessResult, PdfError> {
    let start = std::time::Instant::now();

    // Step 1: Smart detection (fast, no full load)
    let detection = detector::detect_pdf_type_mem(buffer)?;

    let result = match detection.pdf_type {
        PdfType::TextBased => {
            // Step 2: Full extraction with position-aware reading order
            let items = extractor::extract_text_with_positions_mem(buffer)?;
            let markdown = to_markdown_from_items(items, MarkdownOptions::default());

            PdfProcessResult {
                pdf_type: PdfType::TextBased,
                text: None,
                markdown: Some(markdown),
                page_count: detection.page_count,
                processing_time_ms: start.elapsed().as_millis() as u64,
            }
        }
        PdfType::Scanned | PdfType::ImageBased => PdfProcessResult {
            pdf_type: detection.pdf_type,
            text: None,
            markdown: None,
            page_count: detection.page_count,
            processing_time_ms: start.elapsed().as_millis() as u64,
        },
        PdfType::Mixed => {
            let items = extractor::extract_text_with_positions_mem(buffer).ok();
            let markdown = items.map(|i| to_markdown_from_items(i, MarkdownOptions::default()));

            PdfProcessResult {
                pdf_type: PdfType::Mixed,
                text: None,
                markdown,
                page_count: detection.page_count,
                processing_time_ms: start.elapsed().as_millis() as u64,
            }
        }
    };

    Ok(result)
}

#[derive(Debug, thiserror::Error)]
pub enum PdfError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("PDF parsing error: {0}")]
    Parse(String),
    #[error("PDF is encrypted")]
    Encrypted,
    #[error("Invalid PDF structure")]
    InvalidStructure,
}

impl From<lopdf::Error> for PdfError {
    fn from(e: lopdf::Error) -> Self {
        PdfError::Parse(e.to_string())
    }
}
