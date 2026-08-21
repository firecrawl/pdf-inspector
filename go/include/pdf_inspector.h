/*
 * C ABI for pdf-inspector's Go binding.
 *
 * Every function below returns an owned, NUL-terminated JSON string that
 * the caller MUST release with pdfinspector_free_string. Passing NULL to
 * pdfinspector_free_string is a no-op; passing anything else is undefined
 * behavior, same as free().
 *
 * Two argument shapes cover every operation:
 *   - (data, len): operations with no options (classify, extract_text).
 *   - (data, len, params_json): everything else. params_json is a
 *     NUL-terminated UTF-8 JSON string (NULL means "use every default");
 *     see go/src/params.rs for the accepted shape per function and
 *     go/src/results.rs for the returned envelope shape. Both are also
 *     documented on each function's doc comment in go/src/lib.rs.
 *
 * This header is hand-written, not cbindgen-generated: the ABI surface is
 * intentionally small and JSON-payload-based, so there is no C struct
 * layout to keep in sync across the FFI boundary.
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

/* Process a PDF's bytes with full extraction: detect type, extract text,
 * and convert to Markdown. params_json: {"pages": [0, 2]} (0-indexed,
 * NULL/{} for every page). */
char *pdfinspector_process_pdf(const unsigned char *data, size_t len,
                                const char *params_json);

/* Fast detection only -- no text extraction or markdown. Same result shape
 * as pdfinspector_process_pdf with markdown always null. No params. */
char *pdfinspector_detect_pdf(const unsigned char *data, size_t len);

/* Process a PDF through native extraction with selective OCR. params_json
 * (all fields optional, defaulting to mode "auto"): {"mode":
 * "off"|"auto"|"force", "page_numbers": [1, 2], "password": "...",
 * "dpi": 150.0, "minimum_confidence": 0.0,
 * "hosted_recommendation_confidence": 0.5, "model_directory": "...",
 * "offline": false}. page_numbers is 1-indexed. "off" exercises the full
 * result/provenance contract without requiring PDFium or an ONNX Runtime
 * library; "auto"/"force" need those available on the host at runtime (see
 * go/README.md). */
char *pdfinspector_process_pdf_with_ocr(const unsigned char *data, size_t len,
                                         const char *params_json);

/* Extract per-page markdown with layout classification metadata.
 * params_json: {"pages": [0, 2]} (0-indexed, NULL/{} for every page). */
char *pdfinspector_extract_pages_markdown(const unsigned char *data,
                                           size_t len,
                                           const char *params_json);

/* Extract text with position/style information. params_json: {"pages":
 * [0, 2]} (0-indexed, NULL/{} for every page). */
char *pdfinspector_extract_text_with_positions(const unsigned char *data,
                                                size_t len,
                                                const char *params_json);

/* Extract structure-tree element references (page, MCID, role) from a
 * tagged PDF; empty list for untagged PDFs. params_json: {"pages": [1, 3]}
 * (1-indexed, matching TextItem.page; NULL/{} for every page). */
char *pdfinspector_extract_structure_elements(const unsigned char *data,
                                               size_t len,
                                               const char *params_json);

/* Extract text within bounding-box regions. params_json:
 * {"page_regions": [{"page": 0, "regions": [[x1,y1,x2,y2], ...]}, ...]}
 * (0-indexed pages, PDF points, top-left origin). */
char *pdfinspector_extract_text_in_regions(const unsigned char *data,
                                            size_t len,
                                            const char *params_json);

/* Extract markdown tables within bounding-box regions. Same params_json
 * shape as pdfinspector_extract_text_in_regions. */
char *pdfinspector_extract_tables_in_regions(const unsigned char *data,
                                              size_t len,
                                              const char *params_json);

/* Detect a vector ruled-line / rectangle grid inside one page region.
 * params_json: {"page_idx": 0, "region_pdf_pt_bbox": [x1,y1,x2,y2],
 * "render_dpi": 200.0}. Returns {"ok":true,"found":bool,"result":...}. */
char *pdfinspector_detect_vector_grid_in_region(const unsigned char *data,
                                                 size_t len,
                                                 const char *params_json);

/* Extract markdown tables using externally-supplied structure recovery
 * (e.g. an SLANet/TSR model's output). params_json: {"inputs": [{"page":0,
 * "crop_pdf_pt_bbox":[x1,y1,x2,y2], "render_dpi":200.0,
 * "structure_tokens":[...], "cell_bboxes":[[...],...]}, ...]}. */
char *pdfinspector_extract_tables_with_structure(const unsigned char *data,
                                                  size_t len,
                                                  const char *params_json);

/* Lower-level sibling of pdfinspector_extract_tables_with_structure:
 * returns resolved cells instead of rendered markdown. Same params_json
 * shape. */
char *
pdfinspector_extract_tables_with_structure_cells(const unsigned char *data,
                                                  size_t len,
                                                  const char *params_json);

/* Auto-fallback variant of pdfinspector_extract_tables_with_structure: runs
 * the TSR-hybrid path and falls back to heuristic extraction on flagged
 * inputs. Same params_json shape. */
char *
pdfinspector_extract_tables_with_structure_auto(const unsigned char *data,
                                                 size_t len,
                                                 const char *params_json);

/* Release a string returned by any function above. */
void pdfinspector_free_string(char *s);

#ifdef __cplusplus
}
#endif

#endif /* PDF_INSPECTOR_GO_H */
