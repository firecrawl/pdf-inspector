#!/usr/bin/env bash
# Cloud Agent install script for pdf-inspector.
#
# Idempotent, non-interactive setup that prepares the full local development
# experience: the Rust release binaries (pdf2md, detect-pdf) plus the sibling
# pdf-evals regression suite that drives them. Safe to run repeatedly and
# against cached state.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

echo "==> Installing system packages (python venv + qpdf)"
# python3-venv: create the pdf-evals virtualenv. qpdf: used when trimming
# fixture PDFs for the corpus (see pdf-evals CLAUDE.md). Both are cheap and
# absent from the default image.
if command -v sudo >/dev/null 2>&1; then
  sudo apt-get update -qq
  sudo apt-get install -y -qq python3.12-venv qpdf
else
  apt-get update -qq
  apt-get install -y -qq python3.12-venv qpdf
fi

echo "==> Building release binaries with the pinned toolchain"
# rust-toolchain.toml pins the channel (1.98.0); cargo auto-installs it on the
# first invocation. This produces target/release/{pdf2md,detect-pdf}.
cargo build --release

# --- pdf-evals regression suite (sibling repository dependency) -------------
EVALS_DIR="$(dirname "$REPO_ROOT")/pdf-evals"
if [ -d "$EVALS_DIR" ]; then
  echo "==> Setting up pdf-evals at $EVALS_DIR"
  cd "$EVALS_DIR"

  if [ ! -d .venv ]; then
    python3 -m venv .venv
  fi
  # shellcheck disable=SC1091
  source .venv/bin/activate
  python -m pip install --quiet --upgrade pip

  # Install only the core benchmark/scoring dependencies (everything above the
  # "optional provider dependencies" marker in requirements.txt). The optional
  # cloud-OCR providers (docling/torch, pymupdf4llm, markitdown, llama-cloud)
  # are heavy and unnecessary for the local pdf-inspector dev + regression flow.
  CORE_REQS="$(sed '/optional provider dependencies/q' requirements.txt \
    | grep -vE '^[[:space:]]*#' | grep -vE '^[[:space:]]*$')"
  # shellcheck disable=SC2086
  pip install --quiet $CORE_REQS

  # Point the local provider at the freshly built binary. config.yaml is
  # git-ignored, so writing it here is safe and keeps future shells working
  # without needing PDF_INSPECTOR_BINARY. detect-pdf is auto-resolved from the
  # same directory as pdf2md.
  cat > config.yaml <<EOF
# Written by pdf-inspector/scripts/cloud-agent-install.sh
providers:
  local:
    binary: $REPO_ROOT/target/release/pdf2md
EOF
  deactivate
  echo "==> pdf-evals ready"
else
  echo "==> pdf-evals not found next to pdf-inspector; skipping eval setup"
fi

echo "==> Install complete"
