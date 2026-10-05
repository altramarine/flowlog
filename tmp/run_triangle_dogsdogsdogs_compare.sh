#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_dir="$(cd -- "$script_dir/.." && pwd)"
edge_file="${TRIANGLE_EDGE_FILE:-/users/yp_wei/wcoj_by_sql/datasets/wiki-topcats.csv}"
workers="${TRIANGLE_WORKERS:-32}"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
run_dir="${TRIANGLE_COMPARE_RUN_DIR:-$script_dir/triangle-dogsdogsdogs-compare/$stamp}"
flowlog_dir="$run_dir/flowlog"
dogs_dir="$run_dir/dogsdogsdogs"
dogs_project="$script_dir/dogsdogsdogs-triangle-topcats"
dogs_target="$repo_dir/target/dogsdogsdogs-triangle-topcats"

if [[ ! -f "$edge_file" ]]; then
    printf 'missing edge input: %s\n' "$edge_file" >&2
    exit 1
fi

mkdir -p "$flowlog_dir/build" "$dogs_dir"
printf 'edge_file=%s\nworkers=%s\n' "$edge_file" "$workers" > "$run_dir/config.txt"

cargo build --release --manifest-path "$repo_dir/Cargo.toml" \
    --target-dir "$repo_dir/target" -p flowlog-compiler \
    2>&1 | tee "$flowlog_dir/compiler-build.log"

FLOWLOG_RUNTIME_PATH="$repo_dir/flowlog-runtime" \
    "$repo_dir/target/release/flowlog-compiler" "$script_dir/triangle.dl" \
    --mode batch -F "$(dirname -- "$edge_file")" \
    -B "$flowlog_dir/build" -T "$repo_dir/target" -o "$flowlog_dir/triangle" \
    2>&1 | tee "$flowlog_dir/compile.log"

/usr/bin/time -v -o "$flowlog_dir/time.txt" \
    "$flowlog_dir/triangle" -F "$(dirname -- "$edge_file")" -w "$workers" \
    2>&1 | tee "$flowlog_dir/run.log"

cargo build --release --locked --manifest-path "$dogs_project/Cargo.toml" \
    --target-dir "$dogs_target" \
    2>&1 | tee "$dogs_dir/build.log"

/usr/bin/time -v -o "$dogs_dir/time.txt" \
    "$dogs_target/release/dogsdogsdogs-triangle-topcats" \
    "$edge_file" 0 -w "$workers" \
    2>&1 | tee "$dogs_dir/run.log"

printf 'results: %s\n' "$run_dir"
