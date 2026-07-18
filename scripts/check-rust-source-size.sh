#!/bin/sh
set -eu

# Fail if any tracked Rust source file exceeds the per-file line cap.
# The cap is intentionally stricter than upstream Harness (520) at the
# request of the AFI maintainers: 420 physical lines per `.rs` file.
# Splitting oversized files to satisfy this cap is deferred production work;
# this gate only reports offenders so the validation lane is in place first.

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
max_lines=420
violations=$(mktemp "${TMPDIR:-/tmp}/afi-rust-size.XXXXXX")
trap 'rm -f "$violations"' EXIT HUP INT TERM

find "$repo_root/src" "$repo_root/tests" \
    -type f -name '*.rs' -not -path '*/target/*' \
    -exec wc -l {} + \
    | awk -v max="$max_lines" -v root="$repo_root/" '
      $2 != "total" && $1 > max {
        count = $1
        sub(/^[[:space:]]*[0-9]+[[:space:]]+/, "")
        if (index($0, root) == 1) {
          $0 = substr($0, length(root) + 1)
        }
        printf "%4s  %s\n", count, $0
      }
    ' > "$violations"

if [ -s "$violations" ]; then
    printf 'Rust source file length limit exceeded (max %s lines):\n' "$max_lines" >&2
    sort -nr "$violations" >&2
    exit 1
fi

printf 'All Rust source files within the %s-line limit.\n' "$max_lines"
