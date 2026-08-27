# Native .NET ABI

This crate is an internal `cdylib` consumed by `Firecrawl.PdfInspector`.
Consumers should use the managed NuGet API rather than call these exports
directly.

The ABI version is returned by `pdf_inspector_abi_version`. PDF data is passed
as a pointer and length. Options and structured results use UTF-8 JSON matching
the managed/WASM contracts. Every operation returns:

```c
struct PdfInspectorResult {
    int32_t status;
    uint8_t *data;
    size_t len;
};
```

Status `0` is success. Non-zero results contain a UTF-8 error message. The
caller must release every non-null result buffer exactly once with
`pdf_inspector_free_result(data, len)`. Rust panics are caught and returned as
status `4`; they never unwind across the C boundary.
