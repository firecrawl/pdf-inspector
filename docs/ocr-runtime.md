# OCR runtime setup

Selective OCR is available from the Rust library and CLI, Python, and Node.js.
Clean native-text documents do not load an OCR dependency or download a model.
When `auto` routes at least one page, the process needs PDFium, ONNX Runtime,
and the pinned PP-OCRv6 Small model set.

## Validated versions

The reproducible runtime path uses these builds:

- [Firecrawl PDFium `native-v7988`](https://github.com/firecrawl/pdfium-rs/releases/tag/native-v7988),
  containing PDFium `153.0.7988.0`
- [ONNX Runtime `1.27.0`](https://pypi.org/project/onnxruntime/1.27.0/)
- PP-OCRv6 Small artifact revision `oar-ocr-v0.7.0`

Use these versions for the reproducible path. Other compatible shared-library
builds may work, but are not part of the release smoke test.

## Install the shared libraries

Download and extract the matching archives. The ONNX Runtime v1.27.0 GitHub
release archives are no longer available, so take the shared library from the
`onnxruntime` 1.27.0 wheel on PyPI. A wheel is a zip file; the library is under
`onnxruntime/capi/`:

| Platform | PDFium asset | ONNX Runtime wheel | Library in the wheel |
|---|---|---|---|
| Linux x64 | `firecrawl-pdfium-linux-x64.tgz` | `onnxruntime-1.27.0-cp312-cp312-manylinux_2_27_x86_64.manylinux_2_28_x86_64.whl` | `libonnxruntime.so.1.27.0` |
| Linux ARM64 | `firecrawl-pdfium-linux-arm64.tgz` | `onnxruntime-1.27.0-cp312-cp312-manylinux_2_27_aarch64.manylinux_2_28_aarch64.whl` | `libonnxruntime.so.1.27.0` |
| macOS Apple Silicon | `firecrawl-pdfium-mac-arm64.tgz` | `onnxruntime-1.27.0-cp312-cp312-macosx_14_0_arm64.whl` | `libonnxruntime.1.27.0.dylib` |
| Windows x64 | `firecrawl-pdfium-win-x64.tgz` | `onnxruntime-1.27.0-cp312-cp312-win_amd64.whl` | `onnxruntime.dll` |

For example, on Linux x64:

```bash
unzip -j onnxruntime-1.27.0-cp312-cp312-manylinux_2_27_x86_64.manylinux_2_28_x86_64.whl \
  'onnxruntime/capi/libonnxruntime.so*' -d onnxruntime
```

The PDFium release publishes `SHA256SUMS`, build provenance, license files,
and an SPDX document for every platform archive. PyPI publishes a SHA-256
digest with each ONNX Runtime wheel.

Point pdf-inspector at the extracted shared libraries when they are not on the
platform library search path:

```bash
export PDFIUM_LIB_PATH=/absolute/path/to/libpdfium.so
export ORT_DYLIB_PATH=/absolute/path/to/libonnxruntime.so
pdf2md scan.pdf --ocr auto --json
```

On macOS the filenames end in `.dylib`. On Windows, use PowerShell and point
the variables at `pdfium.dll` and `onnxruntime.dll`:

```powershell
$env:PDFIUM_LIB_PATH = "C:\absolute\path\to\pdfium.dll"
$env:ORT_DYLIB_PATH = "C:\absolute\path\to\onnxruntime.dll"
pdf2md scan.pdf --ocr auto --json
```

The native extraction packages also support platforms without these exact
runtime assets. In particular, the Python package has an Intel macOS wheel,
but ONNX Runtime 1.27.0 does not publish an Intel macOS build; local OCR on
that target requires a compatible custom ONNX Runtime build.

The full OCR path is exercised end to end on Linux x64 in CI. macOS and
Windows compile and run the feature's platform-independent tests, while their
external-runtime paths should be treated as preview until equivalent smoke
jobs are added.

## Model cache and offline mode

The first routed page downloads and SHA-256-verifies three pinned artifacts:
the detection model, recognition model, and character dictionary. Together
they are about 31 MB. They are stored below the platform cache directory.
Set `PDF_INSPECTOR_MODEL_CACHE` to choose a managed cache root.

For hermetic deployments, populate the model directory ahead of time and use
the language-specific offline option:

- CLI: `--ocr-offline --ocr-model-dir /models/pp-ocrv6-small`
- Rust: `ModelDownloadPolicy::Offline` with `OcrOptions::model_directory`
- Python: `offline=True, model_directory="/models/pp-ocrv6-small"`
- Node.js: `offline: true, modelDirectory: "/models/pp-ocrv6-small"`

The model artifacts come from
[`GreatV/oar-ocr`](https://github.com/GreatV/oar-ocr/releases/tag/v0.7.0),
whose OCR implementation and upstream PaddleOCR project use Apache-2.0
licensing. Models are downloaded at runtime and are not embedded in any
pdf-inspector package.

## Hosted fallback boundary

`pages_recommending_hosted` is available after the local pipeline completes.
It marks pages whose completed OCR result is empty, low-confidence, or still
appears incomplete.

Setup and execution failures happen before that result exists. A missing or
incompatible PDFium/ONNX Runtime library, failed model acquisition, or OCR
execution error is returned as an error. A downstream integration that has a
hosted parser should catch that error and route the document to the hosted
path. This keeps deployment problems distinct from page-quality judgments.

In `auto`, documents with no routed pages return successfully without touching
PDFium, ONNX Runtime, the model cache, or the network.
