#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_dir="$(cd -- "$script_dir/.." && pwd)"
fact_dir="${1:-/users/yp_wei/wcoj_by_sql/datasets}"
workers="${2:-32}"
edge_file="${FLOWLOG_EDGE_FILE:-$fact_dir/wiki-topcats.csv}"
run_label="${FLOWLOG_RUN_LABEL:-topcats}"
include_baseline="${FLOWLOG_INCLUDE_BASELINE:-1}"
run_root="$script_dir/${run_label}-5cycle-compare/$(date -u +%Y%m%dT%H%M%SZ)"
input_dir="$run_root/input"

if [[ ! -f "$edge_file" ]]; then
    printf 'missing edge input: %s\n' "$edge_file" >&2
    exit 1
fi

mkdir -p "$input_dir"
ln -s "$edge_file" "$input_dir/wiki-topcats.csv"
ln -s "$edge_file" "$input_dir/Arc.csv"

cargo build --release --manifest-path "$repo_dir/Cargo.toml" \
    --target-dir "$repo_dir/target" -p flowlog-compiler

compile_case() {
    local label="$1"
    local program="$2"
    local case_dir="$run_root/$label"

    mkdir -p "$case_dir"
    FLOWLOG_RUNTIME_PATH="$repo_dir/flowlog-runtime" \
        "$repo_dir/target/release/flowlog-compiler" "$program" \
        --mode batch -F "$input_dir" \
        -B "$case_dir/build" -T "$repo_dir/target" -o "$case_dir/program" \
        2>&1 | tee "$case_dir/compile.log"
}

run_case() {
    local label="$1"
    local case_dir="$run_root/$label"
    local status

    set +e
    /usr/bin/time -v -o "$case_dir/time.txt" \
        "$case_dir/program" -F "$input_dir" -w "$workers" \
        2>&1 | tee "$case_dir/run.log"
    status="${PIPESTATUS[0]}"
    set -e
    printf '%s\n' "$status" > "$case_dir/exit-status.txt"
}

if [[ -n "${FLOWLOG_CASES:-}" ]]; then
    IFS=, read -r -a case_labels <<< "$FLOWLOG_CASES"
elif [[ "$include_baseline" == "1" ]]; then
    case_labels=(baseline before after)
else
    case_labels=(before after)
fi

for label in "${case_labels[@]}"; do
    case "$label" in
        baseline) compile_case baseline "$script_dir/5cycle.baseline.dl" ;;
        before) compile_case before "$script_dir/5cycle.original.dl" ;;
        after) compile_case after "$script_dir/5cycle.indexed.dl" ;;
        *)
            printf 'unknown case: %s\n' "$label" >&2
            exit 2
            ;;
    esac
done

for label in "${case_labels[@]}"; do
    run_case "$label"
done

printf 'results: %s\n' "$run_root"
for label in "${case_labels[@]}"; do
    printf '\n[%s]\n' "$label"
    cat "$run_root/$label/exit-status.txt"
    rg 'Elapsed \(wall clock\)|Maximum resident set size|Exit status' \
        "$run_root/$label/time.txt" || true
done
