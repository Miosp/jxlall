#!/usr/bin/env bash
# Converts the fixtures with a built binary and checks that each output is
# a JPEG XL file (container or bare codestream signature).
set -euo pipefail

bin=$1
here=$(dirname "$0")
out=$(mktemp -d)

"$bin" --version
"$bin" -o "$out" "$here/gradient.png" "$here/plasma.jpg"

for name in gradient plasma; do
    file="$out/$name.jxl"
    magic=$(od -An -tx1 -N12 "$file" | tr -d ' \n')
    case $magic in
        0000000c4a584c200d0a870a | ff0a*) echo "ok: $name.jxl" ;;
        *) echo "not a JPEG XL file: $file ($magic)" >&2; exit 1 ;;
    esac
done
