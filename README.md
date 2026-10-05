# FlowLog `quicktest`

`quicktest` is an experimental snapshot built from FlowLog upstream commit
[`5d24aaf`](https://github.com/flowlog-rs/flowlog/commit/5d24aaffa357316bd6c6dfac09d8ef6e79aaed9c).
It collects the local work used to inspect FlowLog plans, compare a 5-cycle
rewrite, and compare FlowLog's triangle plan with `differential-dogs3`.
This README deliberately documents the contents of this branch rather than
duplicating upstream's project README.

The FlowLog planner, runtime, and compiler sources are unchanged from that
upstream commit. All branch-specific material is under [`tmp/`](tmp/).

## Branch contents

### 5-cycle split-plan rewrite

The query is a directed 5-cycle over `Arc`:

```datalog
Arc(a, b), Arc(b, c), Arc(c, d), Arc(d, e), Arc(e, a)
```

The files preserve each stage of the rewrite.

| File | Purpose |
| --- | --- |
| [`tmp/5cycle.dl`](tmp/5cycle.dl) and [`tmp/5cycle.original.dl`](tmp/5cycle.original.dl) | Original translation of the WCOJ split tree. Some IDB names are reused by alternatives below different splits, so FlowLog unions their contents. |
| [`tmp/5cycle.fixed.dl`](tmp/5cycle.fixed.dl) | First correction: give branch-local intermediate relations separate names and expand the corresponding result rules. |
| [`tmp/5cycle.indexed.dl`](tmp/5cycle.indexed.dl) | Current version used by the benchmark runner. Internal split-tree nodes are numbered. A relation made before a split is shared by its descendants; a relation made after a split carries the leaf index. This preserves common prefix context while preventing cross-branch unions. |
| [`tmp/5cycle.baseline.dl`](tmp/5cycle.baseline.dl) | Baseline plan: materialize `R1 join R2 join R3`, materialize `R4 join R5`, then join the two. |

[`tmp/run_topcats_5cycle_compare.sh`](tmp/run_topcats_5cycle_compare.sh) compiles and
runs `baseline`, `before` (`5cycle.original.dl`), and `after`
(`5cycle.indexed.dl`) in batch mode. It records the compiler output, program
output, exit status, and GNU `time -v` report in a timestamped directory. The
input path and worker count are positional arguments; the default data path is
the local WCOJ dataset directory.

Small result records from the already-run 5-cycle comparisons are retained in
`tmp/*-5cycle-compare/`. They include runs on Topcats, 70% and 30% induced
Topcats subgraphs, BerkStan, and Skitter. The full Topcats record at
[`tmp/topcats-5cycle-compare/20260928T050808Z/summary.txt`](tmp/topcats-5cycle-compare/20260928T050808Z/summary.txt)
shows that both split-plan variants terminated with allocation failure on that
machine, while the baseline completed in 8.13 seconds. These are observations
from one host configuration, not a performance claim for the rewrite.

[`tmp/make_induced_subgraph.py`](tmp/make_induced_subgraph.py) makes the induced
Topcats subgraphs. Their small metadata files are retained as
[`tmp/wiki-topcats-70pct-seed0.stats.txt`](tmp/wiki-topcats-70pct-seed0.stats.txt)
and [`tmp/wiki-topcats-30pct-seed0.stats.txt`](tmp/wiki-topcats-30pct-seed0.stats.txt).

### Triangle: FlowLog and `differential-dogs3`

[`tmp/triangle.dl`](tmp/triangle.dl) is the FlowLog triangle program. Three
small standalone Rust projects exercise the corresponding dogs3 operator:

| Directory | Program |
| --- | --- |
| [`tmp/dogsdogsdogs-topcats-quicktest`](tmp/dogsdogsdogs-topcats-quicktest) | Initial dogs3 Topcats quick test. |
| [`tmp/dogsdogsdogs-triangle-topcats`](tmp/dogsdogsdogs-triangle-topcats) | Triangle comparison with the separate-index/operator experiment. |
| [`tmp/dogsdogsdogs-triangle-topcats-batch`](tmp/dogsdogsdogs-triangle-topcats-batch) | Batch-mode comparison. It loads one relation, builds forward and reverse dogs3 indexes, uses `count` to choose an extension direction, then `propose` and `validate` to produce triangles. |

The runners are:

- [`tmp/run_dogsdogsdogs_topcats_quicktest.sh`](tmp/run_dogsdogsdogs_topcats_quicktest.sh)
- [`tmp/run_dogsdogsdogs_triangle_topcats.sh`](tmp/run_dogsdogsdogs_triangle_topcats.sh)
- [`tmp/run_triangle_dogsdogsdogs_compare.sh`](tmp/run_triangle_dogsdogsdogs_compare.sh)
- [`tmp/run_triangle_dogsdogsdogs_batch_compare.sh`](tmp/run_triangle_dogsdogsdogs_batch_compare.sh)

The recorded results are collected in
[`tmp/triangle-dogsdogsdogs-results.md`](tmp/triangle-dogsdogsdogs-results.md).
Every listed run used `wiki-topcats.csv` and 32 workers; both implementations
reported 27,691,482 triangles in every run. The batch-aligned run recorded
7.47 s and 12.12 GiB maximum RSS for FlowLog, and 7.70 s and 22.97 GiB for
dogs3. The results document distinguishes that batch comparison from the
earlier operator experiment, whose construction is different.

### DOOP cyclic-rule profile

[`tmp/doop-zxing-cyclic-profile.md`](tmp/doop-zxing-cyclic-profile.md) and
[`tmp/doop-zxing-cyclic-profile.csv`](tmp/doop-zxing-cyclic-profile.csv) retain
the processed profile for the ZXing DOOP batch run: 16 workers, string
interning, 4.28 s wall time, 1.57 GiB peak RSS, and 4,024,065 `VarPointsTo`
facts. The report separates transformations exclusive to a source rule from
shared transformations, so overlapping rule totals are not added together.

## Reproducing the runnable experiments

Build the FlowLog compiler first:

```bash
cargo build --release -p flowlog-compiler
```

Run the 5-cycle comparison with an edge CSV and worker count:

```bash
bash tmp/run_topcats_5cycle_compare.sh /path/to/facts 32
```

The directory must contain the edge file selected by `FLOWLOG_EDGE_FILE`, or
the default `wiki-topcats.csv`. The runner makes an `Arc.csv` symlink in its
own temporary input directory.

For the batch triangle comparison, point the environment at an edge file:

```bash
TRIANGLE_EDGE_FILE=/path/to/wiki-topcats.csv \
TRIANGLE_WORKERS=32 \
bash tmp/run_triangle_dogsdogsdogs_batch_compare.sh
```

Each runner writes a fresh timestamped result directory under `tmp/` and does
not overwrite the checked-in records.

## Files intentionally kept local

The original working directory also contains raw graph inputs, input symlinks,
generated compiler build trees, Cargo target directories, and generated
executables. They are preserved locally but are ignored on this branch:

- `tmp/as-skitter-undirected.csv` (about 298 MB)
- `tmp/wiki-topcats-70pct-seed0.csv` (about 207 MB)
- `tmp/wiki-topcats-30pct-seed0.csv` (about 39 MB)
- generated `build/`, `target/`, `program`, and `triangle` outputs

The first two files exceed GitHub's 100 MB per-file limit. Keeping generated
artifacts out of the branch also makes the checked-in scripts and source files
the authoritative way to reproduce a run. Nothing in the local experiment
directory is deleted by this branch.
