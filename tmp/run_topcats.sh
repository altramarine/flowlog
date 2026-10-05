#!/usr/bin/env bash
set -euo pipefail

# Usage: ./tmp/run_topcats.sh [directory-containing-wiki-topcats.csv] [workers]
# A, B, and C all read R; Tri counts ordered tuples without dividing by three.
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_dir="$(cd -- "$script_dir/.." && pwd)"
fact_dir="${1:-/users/yp_wei/wcoj_by_sql/datasets}"
workers="${2:-32}"
run_dir="$script_dir/topcats-run"
mkdir -p "$run_dir"

cargo build --release --manifest-path "$repo_dir/Cargo.toml" \
    --target-dir "$repo_dir/target" -p flowlog-compiler

FLOWLOG_RUNTIME_PATH="$repo_dir/flowlog-runtime" \
    "$repo_dir/target/release/flowlog-compiler" "$script_dir/triangle.dl" \
    --mode batch -F "$fact_dir" \
    -B "$run_dir/build" -T "$repo_dir/target" -o "$run_dir/triangle" \
    2>&1 | tee "$run_dir/compile.log"

# Measure execution separately from compilation; printsize avoids tuple-file I/O.
/usr/bin/time -v -o "$run_dir/time.txt" \
    "$run_dir/triangle" -F "$fact_dir" -w "$workers" \
    2>&1 | tee "$run_dir/run.log"
cat "$run_dir/time.txt"
