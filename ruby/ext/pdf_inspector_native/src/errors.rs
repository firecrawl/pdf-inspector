use magnus::{exception::ExceptionClass, value::Lazy, Error, Module, RModule, Ruby};

// The `PdfInspector` error hierarchy exposed to Ruby:
//   PdfInspector::Error
//     ├── PdfInspector::EncryptedError
//     ├── PdfInspector::InvalidPdfError
//     └── PdfInspector::ParseError

static PDF_INSPECTOR: Lazy<RModule> = Lazy::new(|ruby| ruby.define_module("PdfInspector").unwrap());

static ERROR: Lazy<ExceptionClass> = Lazy::new(|ruby| {
    ruby.get_inner(&PDF_INSPECTOR)
        .define_error("Error", ruby.exception_standard_error())
        .unwrap()
});

static ENCRYPTED_ERROR: Lazy<ExceptionClass> = Lazy::new(|ruby| {
    ruby.get_inner(&PDF_INSPECTOR)
        .define_error("EncryptedError", ruby.get_inner(&ERROR))
        .unwrap()
});

static INVALID_PDF_ERROR: Lazy<ExceptionClass> = Lazy::new(|ruby| {
    ruby.get_inner(&PDF_INSPECTOR)
        .define_error("InvalidPdfError", ruby.get_inner(&ERROR))
        .unwrap()
});

static PARSE_ERROR: Lazy<ExceptionClass> = Lazy::new(|ruby| {
    ruby.get_inner(&PDF_INSPECTOR)
        .define_error("ParseError", ruby.get_inner(&ERROR))
        .unwrap()
});

/// Forces the `PdfInspector` error hierarchy to be defined on the Ruby side.
///
/// Must be called during extension init: the classes need to exist before
/// any error can be raised, and `Lazy` statics are otherwise only forced on
/// first use.
pub(crate) fn init(ruby: &Ruby) {
    Lazy::force(&PARSE_ERROR, ruby);
}

/// Maps a `pdf_inspector::PdfError` to the corresponding Ruby exception class
/// in the `PdfInspector` error hierarchy, preserving the error message.
pub(crate) fn to_magnus_err(ruby: &Ruby, err: pdf_inspector::PdfError) -> Error {
    let message = err.to_string();

    match err {
        pdf_inspector::PdfError::Encrypted => Error::new(ruby.get_inner(&ENCRYPTED_ERROR), message),
        pdf_inspector::PdfError::NotAPdf(_) | pdf_inspector::PdfError::InvalidStructure => {
            Error::new(ruby.get_inner(&INVALID_PDF_ERROR), message)
        }
        pdf_inspector::PdfError::Parse(_) => Error::new(ruby.get_inner(&PARSE_ERROR), message),
        pdf_inspector::PdfError::Io(_) => Error::new(ruby.get_inner(&ERROR), message),
    }
}
