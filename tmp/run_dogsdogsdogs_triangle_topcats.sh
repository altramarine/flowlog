#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_dir="$(cd -- "$script_dir/.." && pwd)"
project_dir="$script_dir/dogsdogsdogs-triangle-topcats"
edge_file="${DOGS_EDGE_FILE:-/users/yp_wei/wcoj_by_sql/datasets/wiki-topcats.csv}"
max_edges="${DOGS_MAX_EDGES:-0}"
workers="${DOGS_WORKERS:-32}"
target_dir="${DOGS_TARGET_DIR:-$repo_dir/target/dogsdogsdogs-triangle-topcats}"

if [[ ! -f "$edge_file" ]]; then
    printf 'missing edge input: %s\n' "$edge_file" >&2
    exit 1
fi

cargo build --release --manifest-path "$project_dir/Cargo.toml" --target-dir "$target_dir"
exec "$target_dir/release/dogsdogsdogs-triangle-topcats" \
    "$edge_file" "$max_edges" -w "$workers"
