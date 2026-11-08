#!/usr/bin/env bash
# Builds every contract to wasm with cargo-near into res/ (or $NEARPAY_WASM_DIR) and writes
# res/SHA256SUMS.
#
#   scripts/build.sh                 # vault + test mocks, non-reproducible (host toolchain)
#   scripts/build.sh --reproducible  # vault only, reproducible (Docker image pinned by digest in
#                                    # contracts/subscription-vault/Cargo.toml; needs a clean,
#                                    # committed tree) — this is the wasm whose hash is published
#                                    # and deployed (LLD contracts-vault §6)
#
# Requires: cargo-near 0.22.0 and Rust 1.99.1 with wasm32-unknown-unknown (the reproducible image's
# versions; CI uses the same), and Docker for --reproducible.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${NEARPAY_WASM_DIR:-$ROOT/res}"
mkdir -p "$OUT"
cd "$ROOT"

VAULT=contracts/subscription-vault/Cargo.toml
MOCKS=(contracts/mocks/mock-ft/Cargo.toml contracts/mocks/mock-intents/Cargo.toml)

if [[ "${1:-}" == "--reproducible" ]]; then
  if [[ -n "$(git status --porcelain)" ]]; then
    echo "reproducible build needs a clean, committed tree" >&2
    exit 1
  fi
  cargo near build reproducible-wasm --manifest-path "$VAULT" --out-dir "$OUT"
else
  cargo near build non-reproducible-wasm --locked --manifest-path "$VAULT" --out-dir "$OUT"
  for m in "${MOCKS[@]}"; do
    cargo near build non-reproducible-wasm --locked --no-abi --manifest-path "$m" --out-dir "$OUT"
  done
fi

(cd "$OUT" && sha256sum ./*.wasm | sed 's#\./##' | tee SHA256SUMS)
