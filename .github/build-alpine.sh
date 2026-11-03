#!/usr/bin/env bash
# Builds a fully static musl binary for the host architecture inside Alpine,
# runs the tests there and checks that the binary has no dynamic loader.
# Docker's seccomp profile blocks io_uring, so the tests also cover the
# polling fallback. A static binary cannot load ONNX Runtime, so the default
# `ai` feature is left out. The host cargo registry is mounted so CI can
# cache it alongside target/. Usage: build-alpine.sh TARGET
set -euo pipefail

# rust:1.99.1-alpine3.24, pinned by digest.
IMAGE=rust@sha256:b1d5dd5a22a09404d3e798c0cf051b689077360374eb505fa5bb93c479693d87

target=$1
registry="${CARGO_HOME:-$HOME/.cargo}/registry"
mkdir -p "$registry"
trap 'sudo chown -R "$(id -u):$(id -g)" target "$registry"' EXIT
docker run --rm -v "$PWD:/src" -v "$registry:/usr/local/cargo/registry" \
    -w /src -e TARGET="$target" \
    -e RUSTFLAGS="-C target-feature=+crt-static" "$IMAGE" sh -euc '
        apk add --no-cache build-base cmake linux-headers
        cargo build --release --locked --no-default-features --target "$TARGET"
        cargo test --release --locked --no-default-features --target "$TARGET"'

bin="target/$target/release/jxlall"
if readelf -lW "$bin" | grep -q 'program interpreter'; then
    echo "$bin is not statically linked" >&2
    exit 1
fi
