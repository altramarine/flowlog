#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_dir="$(cd -- "$script_dir/.." && pwd)"
project_dir="$script_dir/dogsdogsdogs-topcats-quicktest"
edge_file="${DOGS_EDGE_FILE:-/users/yp_wei/wcoj_by_sql/datasets/wiki-topcats.csv}"
batch_size="${DOGS_BATCH_SIZE:-10000}"
max_edges="${DOGS_MAX_EDGES:-100000}"
workers="${DOGS_WORKERS:-1}"
target_dir="${DOGS_TARGET_DIR:-$repo_dir/target/dogsdogsdogs-topcats-quicktest}"

if [[ ! -f "$edge_file" ]]; then
    printf 'missing edge input: %s\n' "$edge_file" >&2
    exit 1
fi

cargo run --release --manifest-path "$project_dir/Cargo.toml" \
    --target-dir "$target_dir" -- \
    "$edge_file" "$batch_size" "$max_edges" -w "$workers"
