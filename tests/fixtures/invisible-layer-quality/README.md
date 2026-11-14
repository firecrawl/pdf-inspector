Regression inputs for the opt-in invisible OCR layer quality check.

`title-runs.pdf` has 20 separately positioned Hello/World pairs with no
literal spaces. `title-runs-spaced.pdf` is the same page with literal spaces.
Both have a page-sized covering image and readable hidden text.

The four short scan PDFs are unedited Tesseract 5.5.0 output supplied in the
review of https://github.com/firecrawl/pdf-inspector/pull/628. The source
rasters were 1800 x 600 white canvases with centered black Times New Roman,
54 pixels. Tesseract used English, default segmentation and `--dpi 300`.
The rotated controls were rendered upside down; the upright controls used
the same rasters rotated back before running Tesseract.

- Case 2: The physical properties of matter depend on the motion of atoms.
- Case 3: Several physical quantities are used to describe the motion of objects.

The upright controls must return the sentences with no OCR listing. The
rotated controls must remain listed with `invisible_text_layer`.
