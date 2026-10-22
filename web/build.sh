#!/bin/sh
# Builds the browser converter as static files: the converter compiled to
# WebAssembly, and the page in web/site. Needs Rust's wasm32-unknown-unknown target
# and wasm-bindgen-cli 0.2.118, the version of the wasm-bindgen crate.
#
#   web/build.sh [output folder, by default web/dist]
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-"$root/web/dist"}
mkdir -p "$out"
out=$(cd "$out" && pwd)
target=${CARGO_TARGET_DIR:-"$root/target"}

cd "$root"
cargo build --profile wasm --target wasm32-unknown-unknown -p notability-web
rm -rf "$out/pkg"
wasm-bindgen --target web --no-typescript --out-dir "$out/pkg" \
  "$target/wasm32-unknown-unknown/wasm/notability_web.wasm"
cp "$root"/web/site/* "$out/"
