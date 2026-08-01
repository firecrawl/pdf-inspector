/// Controls how far the PDF processing pipeline runs.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum ProcessMode {
    /// Only detect PDF type. Very fast — no text extraction.
    DetectOnly,
    /// Detect type + extract text + compute layout complexity. Skips markdown.
    Analyze,
    /// Full pipeline: detect, extract, convert to markdown (default).
    #[default]
    Full,
}
