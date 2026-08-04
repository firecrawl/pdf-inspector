/*
 * C ABI for pdf-inspector's Go binding.
 *
 * Every function below returns an owned, NUL-terminated JSON string that
 * the caller MUST release with pdfinspector_free_string. Passing NULL to
 * pdfinspector_free_string is a no-op; passing anything else is undefined
 * behavior, same as free().
 *
 * See go/src/lib.rs for the exact envelope shapes:
 *   pdfinspector_classify:     {"ok":true,"result":{...}}  | {"ok":false,"error":"..."}
 *   pdfinspector_extract_text:{"ok":true,"text":"..."}     | {"ok":false,"error":"..."}
 *
 * This header is hand-written, not cbindgen-generated: the ABI surface is
 * intentionally small (three functions, JSON payloads) so there is no
 * struct layout to keep in sync across the FFI boundary.
 */

#ifndef PDF_INSPECTOR_GO_H
#define PDF_INSPECTOR_GO_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Classify a PDF's bytes: type, page count, which pages need OCR, and a
 * confidence score. Fast -- skips text/markdown extraction entirely.
 * `data` must be valid for `len` bytes (or `len` may be 0). Never returns
 * NULL. */
char *pdfinspector_classify(const unsigned char *data, size_t len);

/* Extract plain text from a PDF's bytes (no layout/markdown formatting).
 * `data` must be valid for `len` bytes (or `len` may be 0). Never returns
 * NULL. */
char *pdfinspector_extract_text(const unsigned char *data, size_t len);

/* Release a string returned by either function above. */
void pdfinspector_free_string(char *s);

#ifdef __cplusplus
}
#endif

#endif /* PDF_INSPECTOR_GO_H */
