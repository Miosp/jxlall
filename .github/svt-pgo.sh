#!/usr/bin/env bash
# Collects an SVT-AV1 profile for the release build: builds jxlall with
# instrumented SVT-AV1, encodes the photos in pgo/photos to AVIF at a few
# efforts and merges the raw profiles. Prints the merged profile's path for
# JXLALL_SVT_PGO=use:<path>. Needs JXLALL_SVT_LLVM (see the svt-av1-sys
# build script). Usage: svt-pgo.sh TARGET [cargo flags...]
set -euo pipefail

target=$1
shift
target_dir=${CARGO_TARGET_DIR:-$PWD/target}
work="$target_dir/svt-pgo/$target"
rm -rf "$work"
mkdir -p "$work/raw"
# clang on Windows needs native paths (forward slashes keep them space-free
# in CMake flag lists).
if command -v cygpath >/dev/null; then
    work=$(cygpath -m "$work")
fi

# Efforts around the default (6) that users pick most.
EFFORTS="4 6 8"

JXLALL_SVT_PGO="generate:$work/raw" \
    cargo build --release --locked --target "$target" "$@" >&2
bin="$target_dir/$target/release/jxlall"
# One raw profile per process: instrumented runs on Windows ARM64 crash
# merging into an existing one.
export LLVM_PROFILE_FILE="$work/raw/jxlall-%p.profraw"
for e in $EFFORTS; do
    "$bin" -f avif -e "$e" -o "$work/out-$e" pgo/photos >&2
done

"$JXLALL_SVT_LLVM/llvm-profdata" merge -o "$work/svt.profdata" "$work"/raw/*.profraw
echo "$work/svt.profdata"
