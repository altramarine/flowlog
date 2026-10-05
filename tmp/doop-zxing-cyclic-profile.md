# DOOP zxing cyclic-rule profile

mode=batch; workers=16; --str-intern; --profile; one run
wall=4.28s; internal dataflow=4.219677s; peak RSS=1.57 GiB; VarPointsTo=4,024,065

## Rule pipelines

| rule | all CPU-s | exclusive CPU-s | shared CPU-s |
|---|---:|---:|---:|
| VarPointsTo / AssignCast | 4.783 | 4.756 | 0.027 |
| VarPointsTo / LoadInstanceField | 3.430 | 2.462 | 0.968 |
| VarPointsTo / virtual dispatch | 3.714 | 1.702 | 2.012 |
| VarPointsTo / LoadArray | 1.842 | 0.859 | 0.983 |
| ArrayIndexPointsTo / StoreArray | 1.365 | 0.382 | 0.983 |
| Assign(actual, formal) | 0.263 | 0.200 | 0.062 |
| CallGraphEdge / virtual dispatch | 2.609 | 0.180 | 2.429 |
| Reachable / virtual dispatch | 2.605 | 0.176 | 2.429 |

Exclusive means a transformation fingerprint occurs in no other source rule. All and shared overlap across source-rule DAGs, so they must not be summed.

## Shared families

- array family: 4.686 CPU-s, union of all transformations in the listed rules.
- virtual family: 4.497 CPU-s, union of all transformations in the listed rules.

## Hottest transformations in each rule

### VarPointsTo / AssignCast

- 3.057 CPU-s; exclusive; SemiJoin: V:(LK0, RV1)
- 1.129 CPU-s; exclusive; JoinMap: K:(RV0, LV0), V:(LK0, LV1)
- 0.464 CPU-s; exclusive; Arrange: K:(V0, V1) ← VarPointsTo

### VarPointsTo / LoadInstanceField

- 1.153 CPU-s; exclusive; Join: V:(RV0, LV0)
- 1.064 CPU-s; exclusive; Arrange: K:(V1, V2), V:(V0) ← InstanceFieldPointsTo
- 0.941 CPU-s; shared; Arrange: K:(V1), V:(V0) ← VarPointsTo

### VarPointsTo / virtual dispatch

- 0.941 CPU-s; shared; Arrange: K:(V1), V:(V0) ← VarPointsTo
- 0.652 CPU-s; exclusive; JoinMap: K:(LV0), V:(LK0, RV0)
- 0.529 CPU-s; shared; JoinMap: K:(RV0), V:(LV0)

### VarPointsTo / LoadArray

- 0.941 CPU-s; shared; Arrange: K:(V1), V:(V0) ← VarPointsTo
- 0.169 CPU-s; exclusive; JoinMap: K:(LV0), V:(LK0, LV1, RV0)
- 0.159 CPU-s; exclusive; JoinMap: K:(LV0), V:(LK0, RV0)

### ArrayIndexPointsTo / StoreArray

- 0.941 CPU-s; shared; Arrange: K:(V1), V:(V0) ← VarPointsTo
- 0.116 CPU-s; exclusive; JoinMap: K:(RV0), V:(LV0)
- 0.075 CPU-s; exclusive; JoinMap: K:(LV0), V:(LK0, RV0)

### Assign(actual, formal)

- 0.090 CPU-s; exclusive; JoinMap: K:(LV0, RV0), V:(RV1)
- 0.062 CPU-s; shared; Arrange: K:(V1), V:(V0) ← CallGraphEdge
- 0.053 CPU-s; exclusive; Arrange: K:(V1, V0), V:(V2) ← ActualParam

### CallGraphEdge / virtual dispatch

- 0.941 CPU-s; shared; Arrange: K:(V1), V:(V0) ← VarPointsTo
- 0.529 CPU-s; shared; JoinMap: K:(RV0), V:(LV0)
- 0.338 CPU-s; shared; JoinMap: K:(LV0), V:(RV0)

### Reachable / virtual dispatch

- 0.941 CPU-s; shared; Arrange: K:(V1), V:(V0) ← VarPointsTo
- 0.529 CPU-s; shared; JoinMap: K:(RV0), V:(LV0)
- 0.338 CPU-s; shared; JoinMap: K:(LV0), V:(RV0)
