# Triangle: FlowLog and `differential-dogs3`

All recorded comparisons use `wiki-topcats.csv` with 32 Timely workers. Each
run reported exactly 27,691,482 triangles for both programs. Wall time and
maximum resident set size come from `/usr/bin/time -v`; compilation is outside
the measured command.

## Batch-aligned comparison

The FlowLog program is [`triangle.dl`](../triangle.dl). The dogs3 program is
[`dogsdogsdogs-triangle-topcats-batch`](../dogsdogsdogs-triangle-topcats-batch/).
Both load the relation once, execute a batch computation, and then terminate.
The dogs3 implementation builds forward and reverse `CollectionIndex` values,
uses `count` to select an extension direction, and then applies `propose` and
`validate`.

| Run | FlowLog wall / max RSS | dogs3 wall / max RSS | dogs3 / FlowLog wall time |
| --- | ---: | ---: | ---: |
| [`20260930T170825Z`](20260930T170825Z/) | [7.47 s / 12.12 GiB](20260930T170825Z/flowlog/time.txt) | [7.70 s / 22.97 GiB](20260930T170825Z/dogsdogsdogs/time.txt) | 1.03x |

The corresponding program outputs are
[`FlowLog`](20260930T170825Z/flowlog/run.log) and
[`dogs3`](20260930T170825Z/dogsdogsdogs/run.log). The small
[`config.txt`](20260930T170825Z/config.txt) records the input path and worker
count used for the run.

## Earlier operator experiment

These runs use
[`dogsdogsdogs-triangle-topcats`](../dogsdogsdogs-triangle-topcats/). It first
constructs static forward and reverse dogs3 indexes, then feeds the same edges
through a separate query input collection. It is useful for examining the
operator path, but it does not have the same dataflow construction as the
batch-aligned program above. Treat this table as an operator experiment, not
as a like-for-like batch comparison.

| Run | FlowLog wall / max RSS | dogs3 wall / max RSS | dogs3 / FlowLog wall time |
| --- | ---: | ---: | ---: |
| [`20260930T164955Z`](../triangle-dogsdogsdogs-compare/20260930T164955Z/) | [7.52 s / 12.56 GiB](../triangle-dogsdogsdogs-compare/20260930T164955Z/flowlog/time.txt) | [31.88 s / 27.08 GiB](../triangle-dogsdogsdogs-compare/20260930T164955Z/dogsdogsdogs/time.txt) | 4.24x |
| [`20260930T165446Z`](../triangle-dogsdogsdogs-compare/20260930T165446Z/) | [7.96 s / 12.40 GiB](../triangle-dogsdogsdogs-compare/20260930T165446Z/flowlog/time.txt) | [29.30 s / 26.60 GiB](../triangle-dogsdogsdogs-compare/20260930T165446Z/dogsdogsdogs/time.txt) | 3.68x |
| [`20260930T165732Z`](../triangle-dogsdogsdogs-compare/20260930T165732Z/) | [7.50 s / 11.98 GiB](../triangle-dogsdogsdogs-compare/20260930T165732Z/flowlog/time.txt) | [10.24 s / 30.42 GiB](../triangle-dogsdogsdogs-compare/20260930T165732Z/dogsdogsdogs/time.txt) | 1.37x |

The original runner scripts and all raw program output, exit status, and time
reports remain in the sibling timestamped result directories. These values
are single-host observations and should be reproduced before making a general
performance claim.
