# Design Spec: Document Profiles and Domain-Specific Extraction Specialization

## 1. Overview & Motivation

PDF is inherently a presentation-oriented format composed of low-level drawing and text-positioning operators. Different document genres (e.g., technical books/eBooks, academic papers, financial filings) follow vastly different layout paradigms:
- **eBooks / Technical Books** (e.g., *Mastering The Trade*):
  - Contain author-defined **PDF Outlines (bookmarks)** representing the true chapter/section hierarchy.
  - Body text frequently features standalone bold questions, callouts, and emphasis lines that naive visual heuristics misclassify as `#` / `###` headings.
  - Figure captions (e.g., `Figure 1.1`) should serve as lowest-level outline markers (`######` / H6) rather than full-blown section headings or dropped lines.
  - Rich diagrams and charts often use `/Indexed` (palette-based) color spaces, causing basic image extractors to fail silently.
- **Academic Papers / Financial Reports**:
  - Academic papers lack outlines and rely on multi-column reading orders, numbered section heuristics, and abstract/bibliography protection.
  - Financial reports prioritize line- and rect-based table extraction and column-interleaved data.

This specification introduces a **three-phase specialization architecture** ("Base Engine + Profile Derivations"):
1. **Phase 1: Outline-Authoritative Heading Strategy & Figure H6 Mapping**: Suppress body-bold false positives, enforce outline authority when bookmarks exist, and map figure captions directly to `######` (H6).
2. **Phase 2: Palette/Indexed Color & Expanded Image Decoding**: Support `/Indexed` color space (palette-to-RGB expansion) and 1-bit image masks in `extractor/xobjects.rs`.
3. **Phase 3: Document Profile Architecture & CLI Presets**: Introduce `DocumentProfile` (`General`, `Book`, `Academic`) to encapsulate coordinated presets for heading, layout, and image strategies.

---

## 2. Phase 1: Heading Strategy & Figure H6 Preservation

### 2.1 Problem Statement
Currently in `src/markdown/convert.rs`:
- Heading classification chain: `struct_heading.or(outline_heading).or(heuristic_heading)`.
- If an outline exists but a line is not in the outline, it falls back to `heuristic_heading`.
- In `heuristic_heading`, any standalone bold line gets `score = 0.3 (bold) + 0.2 (standalone) = 0.5 >= 0.5`, promoting hundreds of body emphasis lines, rhetorical questions, and captions to `###` headings.
- Caption lines (`Figure 1.1`) lack pre-filtering, resulting in `### **Figure 1.1**`.

### 2.2 Design Details
1. **`HeadingMode` Enum** (`src/markdown/mod.rs`):
   ```rust
   #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
   pub enum HeadingMode {
       /// Outline-first when outlines exist (suppresses body-font bold heuristics);
       /// falls back to visual heuristics when no outlines are present.
       #[default]
       Auto,
       /// Outlines (and PDF struct tree) are the sole authority for H1..H5 headings.
       /// Heuristic heading detection is disabled if outlines are available.
       OutlinesStrict,
       /// Pure visual and font-size heuristics; outlines are ignored.
       HeuristicOnly,
   }
   ```
2. **Figure Caption H6 Classification** (`src/markdown/classify.rs` & `convert.rs`):
   - Add `pub(crate) fn is_figure_caption_line(text: &str) -> bool`:
     Matches prefixes such as `Figure `, `Fig. `, `Chart `, `Diagram ` followed by digit/reference.
   - In `convert.rs`:
     - If `is_figure_caption_line(plain_trimmed)` is true, assign heading level `Some(6)` (`###### <Figure Caption>`).
     - In `heuristic_heading`, add `&& !is_caption_line(plain_trimmed)` to reject general captions (`Source:`, `Note:`, `Table:`) from becoming headings.
3. **Heading Resolution Logic** (`convert.rs`):
   ```rust
   let figure_heading = if is_figure_caption_line(plain_trimmed) {
       Some(6)
   } else {
       None
   };

   // Under OutlinesStrict with outlines present: heuristic_heading is completely suppressed.
   // Under Auto with outlines present: body-font bold lines are suppressed from bold_heading_level.
   let effective_heading = struct_heading
       .or(outline_heading)
       .or(figure_heading)
       .or(heuristic_heading);
   ```

### 2.3 CLI & Options
- `MarkdownOptions`: `pub heading_mode: HeadingMode`, with `.with_heading_mode(...)`.
- `PdfOptions`: `.heading_mode(...)`.
- `pdf2md` CLI: `--headings <auto|outlines|heuristic>` (alias `--outlines-only`).

---

## 3. Phase 2: Palette/Indexed Color & Image Extraction Enhancement

### 3.1 Problem Statement
In technical trading books like *Mastering The Trade*, charts and graphs are stored as `/Indexed` PDF XObjects (a base color space + a color palette lookup table).
In `src/extractor/xobjects.rs`, `extract_image_from_stream` only decodes direct `DeviceRGB`, `DeviceGray`, `DeviceCMYK`, `DCTDecode`, and `JPXDecode`. Any stream using `/Indexed` color space or 1-bit component depth returns `None`, silently discarding the image data.

### 3.2 Design Details
1. **Indexed Color Space Detection** (`parse_cs_array` in `xobjects.rs`):
   Recognize `[/Indexed base hival lookup]`:
   - `base`: resolve to `DeviceRGB` (3 components) or `DeviceGray` (1 component).
   - `hival`: maximum index value $N \le 255$.
   - `lookup`: extract palette byte slice of length $(N + 1) \times \text{components}$ from either direct byte string or lopdf stream reference.
2. **Indexed to RGB Conversion**:
   - For 8-bit index data: each byte is an index $i$. Map to RGB using `lookup[i*3 .. i*3+3]`.
   - Encode expanded RGB buffer to PNG via existing `encode_png(width, height, 2, &rgb)`.
3. **1-Bit / Monochrome Image Support**:
   - For images with `BitsPerComponent == 1`: expand each 1-bit pixel (MSB first) to 8-bit grayscale `0x00` / `0xFF`.
   - Encode to PNG via `encode_png(width, height, 0, &gray)`.
4. **CLI & Fallback Experience**:
   - When `--images` or `--image-dir` is specified, properly extract and save indexed/palette PNG charts.

---

## 4. Phase 3: Document Profile Architecture & Presets

### 4.1 Problem Statement
Callers shouldn't need to pass 10 different flags (`--headings outlines --strip-headers-footers --images ...`) for a standard book. A cohesive `DocumentProfile` concept lets users specify document intent directly.

### 4.2 Design Details
1. **`DocumentProfile` Enum** (`src/markdown/mod.rs` & `src/lib.rs`):
   ```rust
   #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
   pub enum DocumentProfile {
       /// Standard balanced extraction for general documents.
       #[default]
       General,
       /// Optimized for published books, manuals, and eBooks:
       /// - HeadingMode: OutlinesStrict (when outlines exist)
       /// - Figure captions as H6
       /// - Aggressive running header/footer deduplication
       Book,
       /// Optimized for two-column academic and technical papers:
       /// - HeadingMode: Auto (font-tier heuristics)
       /// - Column flow reading order
       Academic,
   }
   ```
2. **Profile Application**:
   - `PdfOptions::with_profile(profile: DocumentProfile)` configures default values across `MarkdownOptions`.
   - Individual options (e.g. `--headings`) override profile defaults.
3. **CLI Integration**:
   - `pdf2md <file> --profile book`
   - Auto-detection hint: If a PDF has outlines covering $\ge 50\%$ of pages and count $\ge 8$, log a suggestion or automatically activate `Book` profile in `Auto` mode.

---

## 5. Testing & Validation Strategy

1. **Unit Tests**:
   - `test_figure_caption_as_h6`: verifies `Figure 1.1: Daily Bar Chart` produces `###### Figure 1.1: Daily Bar Chart`.
   - `test_outlines_strict_suppresses_bold_body`: verifies that standalone bold body text stays in normal paragraphs when `OutlinesStrict` is active.
   - `test_indexed_colorspace_png_extraction`: synthetic PDF stream with `/Indexed /DeviceRGB` palette decodes to valid PNG bytes.
2. **CLI Tests**:
   - Validate `--headings outlines` and `--profile book` arguments in `pdf2md`.
3. **Integration Verification**:
   - Re-extract sample pages of *Mastering The Trade* to verify zero false-positive body headings, clean H6 figure anchors, and extracted PNG charts.
