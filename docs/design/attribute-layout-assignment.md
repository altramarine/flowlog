# Problem: Assigning Physical Attribute Orders Across a Dataflow

## Problem statement

Attribute order is a physical design decision. It affects how updates are
generated, sorted, consolidated, exchanged between workers, merged into
arrangements, and traversed by later operators. Aligning one operator's
layout with its upstream or downstream can substantially reduce work, but
the best order for one edge can be expensive for another.

The rule-head experiment in [PR #418][field-order-pr] chooses a physical
row order from an upstream join, normalizes rows in that order, and restores
the logical head order. The broader problem is to consider
attribute order for **every arrangement and the complete path through the
dataflow**, including inputs, intermediate results, rule heads, recursive
feedback, and outputs.

> Given a planned Datalog dataflow and an expected workload, assign
> physical attribute orders to its records, arrangement keys and values,
> and join outputs to minimize end-to-end execution time, while preserving
> semantics and respecting a memory budget.

The argument order written by the user defines logical attribute bindings;
it does not have to define physical storage order. Input ingestion and
output serialization may also use chosen layouts when their interfaces
allow it. A rule head may retain an optimized physical order through
recursion and downstream consumers instead of immediately restoring the
user-written order. Explicit conversions are needed where a fixed external
format is required.

## What can be assigned?

The logical plan determines which attributes a lookup or join needs. The
physical planner can choose their order:

| Object | Layout decisions |
|---|---|
| Input relation | Stored attribute order after input decoding |
| Arrangement | Order within its key tuple and within its value tuple |
| Join inputs | A common ordered join key, plus each side's value order |
| Join output | Projection and physical order of the retained attributes |
| Rule head | Physical row order used by its union, dedup, and feedback |
| Output relation | Stored and emitted order, subject to interface requirements |

For an arrangement with required key attributes `K` and value attributes
`V`, its layout is an ordered pair of permutations:

```text
Arrangement layout = (order(K), order(V))
```

Reordering a key tuple does not change the equality predicate, provided
both sides of a join use the same logical attributes in the same physical
key order. Key membership and key order are separate decisions: an
attribute required for a lookup cannot simply be moved into the value.

For example:

```prolog
J(a,b,c,d,e,f) :- L(a,b,c,d), R(a,b,e,f).
```

One compatible assignment is:

```text
left arrangement:  K=(b,a), V=(d,c)
right arrangement: K=(b,a), V=(f,e)
join traversal model: (b,a,d,c,f,e)
```

This can favor a consumer needing `K=(b,a,d,c), V=(f,e)`, while a consumer
needing `K=(a,b,e,f), V=(c,d)` favors a different layout. The choice of
`(a,b)` versus `(b,a)` and the orders inside each value tuple must therefore
consider both the source arrangements and the consumers.

### The join-order model needs a scope

`(join key, left value, right value)` is a useful model of local join
enumeration, especially for a straightforward value cross product. The
orders inside all three tuples are meaningful choices. Changing an input
arrangement's value order changes match traversal; changing the output
tuple's field positions alone changes the layout in which those matches
are buffered and sorted. These are related but distinct decisions.

Actual incremental joins process multiple batches and timestamped
histories. Larger histories may replay updates from either side according
to time, rather than always using a fixed left-outer/right-inner loop.
Projection can also discard the leading join key or value fields, breaking
the ordering inherited from traversal. Output buffers still sort and
consolidate their chunks. Layout alignment should therefore be modeled as
a potential cost reduction, with explicit order properties where they
are guaranteed.

Arrangement maintenance sorts and consolidates updates and merges sorted
batches into a trace. An arranged join traverses two such traces with
merge-style cursors. Relevant costs include output-buffer sorting, batch
merging, history access, and worker exchange, not just tuple construction.

## Examples motivating the problem

### 1. DOOP/Jython: the head layout can match the producing join

Consider the points-to propagation rule:

```prolog
VarPointsTo(heap,to) :- Assign(from,to), VarPointsTo(heap,from).
```

With `Assign` on the left, the join inputs have:

```text
left:  K=(from), V=(to)
right: K=(from), V=(heap)
```

After dropping `from`, `(to,heap)` follows the left-value/right-value
model, while the user-written head is `(heap,to)`.

```text
experimental physical path:
join -> (to,heap) -> ordinary dedup -> restore (heap,to)

broader layout opportunity:
join -> (to,heap) -> ordinary dedup -> retain that layout for consumers
```

In the full-suite comparison described below, DOOP/Jython execution time
decreased from approximately **315 seconds to 214 seconds** with the field-order
implementation. This is a whole-program result involving multiple heads;
it is not an isolated timing of this rule or of sorting alone.

The local benefit has a downstream cost. Some consumers need an arrangement
whose complete key is `(heap,var)`. Restoring logical field positions with
a map does not restore the previous sorted order or worker partition.
Earlier DOOP profiles showed that a cheaper VarPointsTo head could coexist
with a more expensive downstream full-key arrangement.

The assignment should account for both kinds of consumers. It may be useful
to keep a variable-first primary layout, use a heap-first view where needed,
or choose another shared layout. Each conversion or additional view has a
cost. Changing the input orientation can also change which retained value
comes first, so a layout preference is conditional on the chosen join plan.

### 2. Scallop: the largest contributor may be a seed, not a join

The Polonius query used by Scallop contains:

```prolog
subset(o1,o2,p) :- subset_base(o1,o2,p).

subset(o1,o3,p) :-
    subset(o1,o2,p),
    subset_base(o2,o3,p),
    o1 != o3.

subset(o1,o2,p2) :-
    subset(o1,o2,p1),
    cfg_edge(p1,p2),
    origin_live_on_entry(o1,p2),
    origin_live_on_entry(o2,p2).
```

The transitive rule joins on `(o2,p)` and retains `p`, the left value `o1`,
and the right value `o3`. It suggests a point-first head layout. The last
semijoin in the CFG propagation rule instead suggests `(o2,p2,o1)`.

But this workload has about **27.4 million seed records**, accounting for
roughly **97% of the input updates to the recursive subset head**. Choosing
a layout from a smaller join also forces the large seed through that
layout. The subsequent consumers need several different keys:

```text
(o2,p), (p), (o1), (o1,p)
```

The previous exploration found that `(o2,p,o1)` improved the `(o2,p)`
arrangement relative to the point-first layout, but made the point-only
arrangement more expensive. It also retained costs at the seed and
recursive head. Restoring the original layout for the entire subset path
performed better in that comparison than restoring only the seed layout.

This example requires weighting all producers and consumers by their work:
seed volume, join updates, recursive iterations, and downstream index
construction. Picking the first eligible join cannot express that trade-off.

### 3. Z3: a locally attractive order can hurt recursive trace access

Abbreviate `Stack_def_use_live_var_at_block_end` as `StackEnd`:

```prolog
StackEnd(prev,used,reg,pos) :-
    StackEnd(block,used,reg,pos),
    !Stack_def_use_ref_in_block(block,reg,pos),
    !Reg_def_use_defined_in_block(block,reg),
    Block_next(prev,_,block).
```

The final join has:

```text
K=(block), LV=(reg,pos,used), RV=(prev)
```

The experimental field-order policy selects `(reg,pos,used,prev)` instead
of the original `(prev,used,reg,pos)`. However, the join key `block` is projected
away: value order within one block does not imply sorted projected output
across different blocks. Incremental history replay adds another source
of variation in emission order.

The head's `prev` becomes the next iteration's `block`. Recursive and later
consumers need block-leading keys such as `(block,reg,pos)`. Previous profiles
showed higher costs in the head arrangement, membership threshold, and
downstream Block indexes after the variable-first layout was introduced.

The assignment must include the cost of the persistent head trace and its
feedback consumers. Improving a local traversal pattern is insufficient
if the chosen layout makes these larger or repeated operations more costly.

### 4. Multiple upstreams: a shared head needs a shared decision

```prolog
Out(c,a) :- R(a,b), S(b,c).
Out(c,a) :- T(c,d), U(d,a).
Out(c,a) :- Seed(c,a).
```

Under the corresponding left/right input orientations, the first join
favors `(a,c)` and the second favors `(c,a)`. The seed has its own input
layout. If these contributions share one head trace, they must agree on
one physical schema before normalization.

Selecting the first join's preferred order can benefit that branch while
reordering the other join and the seed. A downstream consumer can introduce
a third preference. Choosing a shared layout should consider the volume and
order properties of every contributor, rather than treating one upstream
as the representative of the entire relation.

Likewise, a shared intermediate arrangement may serve several joins with
different preferences. The assignment must decide when a single layout is
adequate and when another materialized view is worth its memory and update
cost.

## Objective and constraints

The objective is **end-to-end execution time**, subject to an acceptable
peak-memory budget. Local sorting time or summed worker CPU is evidence
for a cost model, but does not directly equal parallel wall-clock time.

A useful cost model should account for:

- Input volume, duplicate witnesses, and the order of incoming update runs.
- Join output-buffer sorting and consolidation under each proposed layout.
- Arrangement batch construction, merging, and historical trace access.
- Conversions and exchanges between incompatible producer/consumer layouts.
- Repeated work along recursive feedback, including delta and seed volumes.
- Worker load balance, shared arrangements, and additional materialized views.

The assignment must preserve these constraints:

1. Logical attribute bindings, types, projections, predicates, and outputs
   remain semantically equivalent under the physical representation.
2. Both join inputs use compatible key identities and key order.
3. Contributors to a shared union or trace agree on a physical schema, or
   have explicit, correctly costed conversions.
4. Dedup membership still uses the intended projected record. A discarded
   witness or join key cannot be added to its identity merely to preserve
   ordering. Signed multiplicities, aggregates, and timestamps retain their
   meaning.
5. A persistent trace has a consistent interpretation across its lifetime;
   recursive feedback and any alternate views respect that interpretation.
6. Input/output mappings honor any required external schema. Where the
   interface is flexible, its physical order participates in the assignment.

Sorting compatibility and partitioning compatibility must be tracked
separately. Ordinary head dedup hashes a complete physical row; an ordinary
keyed arrangement exchanges by its key. Matching their attribute order
does not automatically put records on the same worker. Permuting a tuple
can itself change its hash and worker placement.

## What a good assignment should achieve

The goal is to choose mutually compatible layouts over the operator graph,
with conversions and additional indexes introduced where their total cost
is justified. It should favor expensive or frequently executed paths,
account for conflicting producers and consumers, and keep beneficial
layouts through rule heads and recursion when possible. The user-written
column order and `(key,left-value,right-value)` traversal model provide
reference layouts, not a universal optimum.

## Dynamic programming design

The design below chooses layouts and physical left/right join orientation
for an existing plan. The join tree, materializations, projections, and
sharing decisions are inputs to this optimization. Changing join
associativity or choosing new sharing parents is a separate search problem.

The optimizer minimizes an additive layout cost. An exact solution means
the minimum of that objective over all legal assignments in the modeled
graph. It does not mean the minimum execution time on an unknown dataset.
In particular, the nominal join traversal order described above is a
modeling assumption, not a global ordering guarantee of Differential
Dataflow's incremental join stream.
The solver's table-memory budget below is also separate from the runtime
memory budget in the broader problem statement.

### 1. The cost abstraction: C[sigma_1 -> sigma_2]

A layout is a lexicographic order of **attribute origins**. Origins identify
fields through renaming and projection; two unrelated fields with the same
name are not the same origin. At a sorting boundary, define

$$
C[\sigma_1 \to \sigma_2]
$$

as the estimated additional reordering work when records generated in
source order $\sigma_1$ are consumed in target order $\sigma_2$. The target
is expressed in source origins through the intervening field mappings.
It may retain only a subset of the source fields.

The abstraction separates the solver from the cost model:

- A legal transition has a nonnegative cost. An illegal assignment has
  cost $+\infty$, or is excluded from the state domain.
- An unchanged comparator has zero additional reordering cost. This
  excludes the fixed cost of scanning, hashing, and maintaining an index.
- Each modeled sorting boundary contributes once. There is no separate
  cost for a pure rename or projection with no sorting boundary.
- Projection costs need not be symmetric. They describe the source
  sequence before records with equal projected values are consolidated.

The initial objective uses unit weights and no additional node cost:

$$
F(\ell)=\sum_{e\in E_{\mathrm{sort}}}
C[\sigma_e(\ell)\to\tau_e(\ell)].
$$

Here $\ell$ is one assignment of all layout and join-orientation variables.
The sum is over physical sorting boundaries, not arbitrary graph edges.
Every consumer has its own contribution; a producer's cost is not divided
by its number of consumers. Additional workload weights could be introduced
later without changing the DP, but are not required by this design.

#### A concrete instance: raw binary-record inversions

One data-independent instance of $C$ is the raw inversion count $I$.
For $m$ distinct source origins, assume all $2^m$ binary records occur
once. Sort them lexicographically by $\sigma_1$, apply the field mapping,
and count pairs whose order is reversed by the target comparator.
Equal projected records do not form a strict inversion. Timestamps and
update weights are outside this synthetic record model; the optimization
does not remove them from runtime records.

This counts **record inversions**, not inversions of the attribute
permutation. It distinguishes changing an early comparison field from
changing a late one:

| Source | Target | Raw I |
|---|---|---:|
| `(a,b,c)` | `(b,a,c)` | 4 |
| `(a,b,c)` | `(a,c,b)` | 2 |
| `(c,a,b)` | `(a,b)` | 6 |
| `(c,b,a)` | `(a,b)` | 8 |

For full permutations of the same fields, let $P_\sigma(x)$ be the set
of fields preceding $x$ in $\sigma$. Then

$$
I(\sigma,\tau)=
\sum_{\substack{x<_\sigma y\\y<_\tau x}}
2^{\,2m-|P_\sigma(x)\cup P_\tau(y)|-4}.
$$

Each term counts record pairs whose first unequal source field is $x$
and first unequal target field is $y$. These classes are disjoint. Thus
the score can be computed without enumerating the binary records.
A direct implementation of this formula visits $O(m^2)$ field pairs.
The DP below treats the evaluation cost as a parameter rather than
assuming a particular implementation of $I$.

Raw $I$ is a sorting surrogate. It is not the comparison count of the
runtime sorter, the cost of trace merging, or a prediction of parallel
wall-clock time. The binary model is rebuilt at each modeled boundary;
it does not propagate real cardinalities or correlations through joins.

#### Projection keeps a virtual suffix

Let $\tau$ list the retained fields, and let $D$ list dropped fields in
their source-relative order. For the raw-I instance, define

$$
C[\sigma\to\tau]=I(\sigma,\tau\mathbin\Vert D).
$$

The suffix is only a device for scoring. It does not become part of the
physical row, arrangement value, or dedup identity. If two records differ
on retained fields, their order is decided before the suffix. If they
agree on retained fields, the suffix preserves their source order and
introduces no extra inversion.

For example:

```text
feed:                 (a,b,c,d,e)
retained comparator:  (c,d,e)
comparison for C:     (a,b,c,d,e) -> (c,d,e,a,b)
```

Consecutive pure projections compose before scoring:

```text
(a,b,c) -> (b,c) -> (c) -> arrangement
one cost: (a,b,c) -> (c,a,b)
```

A real intervening arrangement or dedup is a new boundary and prevents
this composition. Its downstream model starts from the fields that
actually survive that boundary; discarded fields do not reappear.

For the earlier two-input example:

```text
R: K=(k), V=(a,b)
S: K=(k), V=(c,d)
nominal join source: (k,a,b,c,d)
downstream arrangement: K=(b,d), V=(a,c)
comparison for C: (k,a,b,c,d) -> (b,d,a,c,k)
```

Here the conceptual trailing block `V'=(k)` is a virtual suffix. The
legal target keeps all of `K` before all of `V`, with permutations allowed
inside each block. No runtime index retains `k` merely for this score.

### 2. A graph of variables and factors

Represent the assignment problem as a finite **factor graph**. It is a
bipartite, undirected graph: variable nodes hold layout choices, and factor
nodes assign costs or enforce compatibility between those choices.
Dataflow direction remains metadata used to interpret each factor.

For each physical representation, introduce permutation variables for its
key and value blocks. A block of $b$ unconstrained fields has $b!$ states.
An arrangement with key width $k$ and value width $v$ therefore has
$k!v!$ legal layouts, rather than $(k+v)!$. A full-row representation
has $m!$ states unless further constraints apply.

The variables can remain split into blocks, which often produces smaller
factor scopes. The domain size of variable $x$ is denoted $d_x$.
Singleton domains can be substituted immediately.

The graph must express the following identities and choices:

- The two inputs of a join share the same key-permutation variable after
  their equal attributes are mapped to common origins.
- A legal inner join has an orientation variable with at most two states.
  Its nominal source is `K ++ LV ++ RV` or `K ++ RV ++ LV`. Asymmetric
  operators, such as antijoins, keep their required semantic orientation.
- All producers of one union/head agree on its physical layout. Each
  producer has a factor mapping its own origins to that shared layout.
- Recursive feedback reuses the same representation variables. Iterations
  are not unrolled into new optimization variables.
- Two different arrangements of a relation have distinct layout variables,
  even if their field sets happen to agree. Only an actually shared trace
  shares a physical-layout decision.
- External schemas, key membership, and unsupported transformations impose
  domain restrictions or compatibility factors.

A sorting factor $f_e$ depends on every variable needed to determine
$\sigma_e$ and $\tau_e$. Its value is their transition cost. A join can
therefore induce a factor involving both input value permutations, the
shared key permutation, orientation, and the target layout. It cannot in
general be replaced by two independently minimized input costs.

For a simplified join with layout variables $a,b,c$:

```text
dataflow:       A join B -> C

factor graph:  [a] ---- (join factor) ---- [c]
                             |
                            [b]
```

Square brackets denote variables and parentheses denote factors. A factor
can have more than two neighbors; such a star is still a tree.

A shared producer may create a cycle even when the dataflow is a DAG:

```text
dataflow:       Q -> A -> H
                \-> B -> H

factor graph:  [q] -- (fQA) -- [a] -- (fAH) -- [h]
                |                              |
               (fQB) -------- [b] ----------- (fBH)
```

The layout of `q` and the layout of `h` must each be chosen consistently
for both branches. Solving the branches independently loses that coupling.
Consequently, breaking directed recursion alone is insufficient: the DP
needs an **undirected factor-graph forest** after conditioning.

Sharing topology is fixed here. A tree of arrangement-sharing parents is
not automatically a forest in this graph, and the DP does not select
sharing parents unless those choices are explicitly added to the model.

### 3. Exact DP on a forest

For a variable $x$ and adjacent factor $f$, let $M_{x\to f}(s)$ be the
minimum cost on the $x$ side of that edge when $x=s$. Let $N(x)$ and
$N(f)$ denote neighbors. With unary restrictions $u_x$, zero for a legal
state unless a separate unary cost is explicitly modeled:

$$
M_{x\to f}(s)=u_x(s)+
\sum_{g\in N(x)\setminus\{f\}}M_{g\to x}(s).
$$

A factor sends a message by minimizing over all its other variables:

$$
M_{f\to x}(s)=
\min_{\{s_y:y\in N(f)\setminus\{x\}\}}
\left[
f(s,\{s_y\})+
\sum_{y\in N(f)\setminus\{x\}}M_{y\to f}(s_y)
\right].
$$

Choose any variable as a root in each component. Evaluate messages from
leaves toward that root. The component optimum is

$$
\min_s\left[u_r(s)+\sum_{f\in N(r)}M_{f\to r}(s)\right].
$$

Store minimizing choices and backtrack from the winning root state to
recover one consistent layout and orientation assignment. Independent
components contribute a sum of optima. A second message pass is useful
when all conditional optima are wanted, but is unnecessary to recover
one optimal assignment.

#### Transitions can run against dataflow

For `A join B -> C`, root the factor tree at `A` if convenient. The message
to `A` is

$$
M_{J\to A}(a)=\min_{b,c,o}
\left[J(a,b,c,o)+M_{B\to J}(b)+M_{C\to J}(c)+M_{o\to J}(o)\right].
$$

The message from `C` includes its downstream costs. This transition
jointly chooses `B`, `C`, and orientation; it does not freeze `C` at the
locally cheapest layout. The same factor sends a message to `C` by
minimizing over `A`, `B`, and orientation. Reversing the message direction
does not reverse the physical operator or replace $C[\sigma\to\tau]$
by $C[\tau\to\sigma]$.

For an unshared join tree, the familiar forward recurrence is a special
case. Write $q,p,t$ for key, left-value, and right-value permutations:

$$
F_C(\tau)=\min_{q,p,t,o}
\left[F_A(q,p)+F_B(q,t)+
C[q\Vert V_o(p,t)\to\tau]\right],
$$

where $V_0(p,t)=p\Vert t$ and $V_1(p,t)=t\Vert p$.
Both inputs use the same $q$. For projected output, the cost applies the
virtual-suffix rule above.

A union whose producer subtrees are independent has the recurrence

$$
F_H(\tau)=\sum_i\min_{\sigma_i}
\left[F_i(\sigma_i)+C[\sigma_i\to\tau]\right].
$$

Every term uses the same $\tau$. This is a sum of alignment contributions
to one head, not a proposal to insert a separate runtime dedup per producer.
If producer subtrees share variables, use the complete factor graph rather
than adding overlapping subtree costs.

The forest recurrence is exact because removing an edge separates the
remaining variables into independent subproblems conditional on its
boundary variable. Each factor is charged once, and every legal state on
that boundary is considered. This conditional independence is the reason
the algorithm works; dataflow acyclicity by itself does not supply it.

### 4. Exact reductions and transition optimizations

These transformations reduce optimization work. They do not require
changing the runtime operator structure.

#### Compose maps, share identities, and remove fixed states

Compose consecutive pure field mappings before extracting sorting factors.
Represent a shared join-key order by one variable rather than an equality
table over two copies. Substitute singleton or externally pinned states,
drop genuinely unused variables, and solve disconnected components
separately. Keep each nonconstant factor and its cost exactly once.

#### Eliminate unrestricted intermediate full-row layouts

For raw $I$ on the same complete field set, the record-order inversion
distance obeys the triangle inequality. If an intermediate $\zeta$ has
only two incident costs, may choose every permutation, and has no other
constraint or consumer, then

$$
\min_\zeta\left[I(\sigma,\zeta)+I(\zeta,\tau)\right]
=I(\sigma,\tau).
$$

Choosing $\zeta=\tau$ attains the bound. This removes a search variable;
its layout can be recovered afterward. The identity requires the same
field set and compatible cost scales. It does not justify eliminating an
arbitrary projected, pinned, or shared intermediate representation.

#### Separate source blocks when the cost permits it

Raw binary-record $I$ has an additional property. For a fixed target and
fixed block order, the effects of internal permutations of disjoint
source blocks are additive. For a join, this gives

$$
I(q\Vert p\Vert t,\tau)=
c(\tau)+g_K(q,\tau)+g_L(p,\tau)+g_R(t,\tau).
$$

To see why, partition record pairs by the first **source block** on which
they differ. Their source comparison depends only on the permutation in
that block. Fixing the target comparator leaves the permutations of other
blocks irrelevant to that class. The argument also holds for strict
comparison after projection.

One construction uses a reference permutation for every block. Let $c$
be the reference cost and let $g_B$ be the change from permuting only block
$B$. These differences can be negative; their sum with $c$ is the original
nonnegative cost. A source block discarded in its entirety has no internal
permutation effect and can be removed from that factor's scope.

For a fixed shared key permutation $q$, target $\tau$, and orientation,
an independent two-child tree transition can therefore minimize the left
and right value permutations separately:

$$
\begin{aligned}
F_C(\tau)=\min_{q,o}\bigl[&c(q,o,\tau)\\
 &+\min_p\{F_A(q,p)+g_L(p,q,o,\tau)\}\\
 &+\min_t\{F_B(q,t)+g_R(t,q,o,\tau)\}\bigr].
\end{aligned}
$$

This changes a product of the two value-domain sizes into a sum for that
transition. In the general graph, preserve the shared target, key, and
orientation variables in the smaller factors. It is not valid to minimize
each consumer independently or to assume the decomposition for an
arbitrary replacement cost model.

#### Cache local tables and conditional messages

A factor depends only on its scope. Cache its cost table once when it is
small enough, and reuse it across cut assignments. A conditioned message
need only be recomputed when cut variables in its dependency set change.
Keep those variables as message parameters when their combined domain
fits the memory budget. This trades larger tables for fewer repeated DPs.

#### Optional raw-I distance transform

For full permutations of $m$ binary fields, swapping adjacent attribute
positions $i,i+1$, counted from zero, has record-inversion cost

$$
w_i=2^{2m-i-4}.
$$

The shortest-path distance on this weighted permutation graph is exactly
full-field $I$. Record-order Kendall distance gives the lower bound.
Moving each next target field left into its target-prefix position fixes
only record pairs that disagree with the final target order, attaining
that bound.

Consequently, a transition of the form

$$
F(\tau)=\min_\sigma\{G(\sigma)+I(\sigma,\tau)\}
$$

can use a multi-source shortest-path transform over $m!$ permutations.
Initialize legal source states with $G$ and read out legal target states.
Intermediate permutation-graph states are calculation states, not new
physical layouts, so they need not satisfy arrangement K/V constraints.

For a projected target, read out the minimum over all completions
$\tau\Vert\delta$ of its dropped fields. For each source, its own
source-relative dropped suffix attains the projected cost; another suffix
can only add inversions between equal projected records. This makes the
transform exact for the projection convention as well.

The transform can avoid a dense source-by-target table. It does not
remove graph cycles or separator variables. It is an optional cost-specific
optimization; the generic forest DP does not depend on it.

### 5. Find a cutset, then enumerate its assignments

When the factor graph contains undirected cycles, choose a set $S$ of
**variable nodes** such that deleting those nodes and their incident
edges leaves a forest. This is a cycle cutset, not a set of discarded
operators or costs.

For every assignment $s$ to $S$, substitute its states in all factors.
A factor with no remaining variables becomes a scalar cost and is still
included. Run the forest DP on the remaining graph and take

$$
\operatorname{OPT}=\min_{s\in\prod_{x\in S}\mathcal L_x}
\operatorname{ForestOPT}(s).
$$

Every complete assignment has exactly one restriction to $S$, so exhaustive
cut enumeration plus exact forest DP gives an exact solution of the
original finite model. The cut need not be minimum for this guarantee.
Choosing a smaller cut makes solving cheaper; it does not improve the
best attainable objective value.

The number of outer assignments is

$$
P(S)=\prod_{x\in S}d_x.
$$

Prefer a cut with small domain product, equivalently small
$\sum_{x\in S}\log d_x$, rather than few variables. A two-state key
permutation and a 720-state full-row permutation have very different
enumeration costs. The cut may contain key, value, full-row, or orientation
variables. Fixing a logical IDB's row order does not also fix all its
different arrangements.

#### A feasible greedy cut

Let $\beta=|E|-|V|+\text{components}$ be the cycle rank of the current
undirected incidence graph. While $\beta>0$, choose a variable whose
deletion reduces $\beta$, favoring a large reduction per $\log d_x$ paid.
Afterward, remove redundant cut variables whenever the residual graph
remains a forest. This gives a feasible incumbent without a claim of an
approximation ratio.

#### Exact branch and bound

To improve or certify that incumbent:

1. Find a cycle in the remaining incidence graph.
2. Branch on selecting each variable node on that cycle. Every valid
   completion must select at least one of them.
3. Multiply the current domain product by the selected variable's domain.
   Prune when a lower bound cannot beat the incumbent product.
4. If there is no cycle, record the feasible cut.

A lower bound comes from a collection of cycles with disjoint variable
sets: multiply the smallest domain size on each cycle. A cut must pay for
at least one distinct variable per such cycle. Memoizing selected-variable
sets avoids visiting the same subproblem through different branch orders.

Keep factor identities fixed during this search. In particular, do not
dynamically merge equal or contained scopes after conditioning and then
use an old cycle as a branching certificate: changing the graph can
invalidate the argument that the selected cycle must be hit. Exact factor
simplifications may be done before search; verify the forest condition on
the resulting fixed graph.

An exhausted search proves minimum $P(S)$ for this fixed factorization and
choice of variable domains. A bounded search may return an uncertified but
valid cut; exhaustive assignment enumeration over that cut still yields
the optimal layout. Stopping the **layout enumeration** early is different
and forfeits that guarantee.

The smallest domain product need not minimize total solver work. Compare
candidate cuts using their residual transition sizes and peak tables as
well. A larger cut may shrink expensive factors enough to be preferable.

### 6. Complexity and the scope of optimality

Let $X_f$ be the variables in factor $f$, $r_f=|X_f|$, and
$A_f=\prod_{x\in X_f}d_x$. Let $\kappa_f$ be the work needed to evaluate
one entry of its cost table. A direct dense construction costs

$$
B=O\left(\sum_f\kappa_f A_f\right).
$$

For a fixed cut $S$, put $A_f(S)=\prod_{x\in X_f\setminus S}d_x$.
With cached factor values, one inward forest pass has the bound

$$
T(S)=O\left(
\sum_f\max(1,r_f)A_f(S)+
\sum_{x\notin S}\deg(x)d_x
\right).
$$

The factor term includes scanning state combinations and adding incoming
messages; it also includes scalar factors. Variable messages sum incoming
tables. With $Q_{\mathrm{cut}}$ graph-search work, the uncached-message
outer scheme has the bound

$$
O\left(Q_{\mathrm{cut}}+B+P(S)T(S)\right).
$$

If full tables are too large, evaluate entries on demand; then their
evaluation cost reappears in the repeated DP term. Dense factor storage
is $O(\sum_f A_f)$, with additional message tables and backpointers.
Cut assignments can be processed one at a time. A small cut product does
not by itself bound the largest local table or total memory use.

Disconnected components must be counted separately:

$$
\sum_H\left[B_H+P(S_H)T_H(S_H)\right],
$$

plus cut-search work. Multiplying cut domains from unrelated components
creates avoidable repeated work.

#### Factorials in a join transition

For a join with $k$ key fields, $l$ left-value fields, and $r$ right-value
fields, there are at most $2k!l!r!$ nominal source layouts, including
orientation. If its target has $D$ legal layouts, direct forward
enumeration needs

$$
O\left(\kappa(m)Dk!l!r!\right),\qquad m=k+l+r,
$$

with the factor of two suppressed. A target arrangement has
$D=k_{\mathrm{out}}!v_{\mathrm{out}}!$; a free projected row with $p$
distinct fields has $D=p!$. The source width $m$ includes projected-away
fields.

The orientation factor of two is local to each join transition. It does
not require an outer enumeration of all joins' orientation combinations.
When orientation occurs in only one factor, minimize its two choices
inside that factor before cut search. When several factors depend on the
same orientation, keep their choice consistent; minimizing each factor's
orientation independently would solve a different problem.

When the raw-I block decomposition and independent child messages apply,
the forward transition becomes

$$
O\left(\kappa(m)Dk!(l!+r!)\right).
$$

In a budget charging $m$ units per cost evaluation, this is the shorthand
$mDk!(l!+r!)$. That accounting convention must be distinguished from the
actual complexity of the chosen cost oracle. Reverse messages and factors
with other shared interfaces are budgeted from their actual scopes;
the forward formula is not a universal bound on every graph transition.

The optional distance transform instead has $m!$ states and
$(m-1)m!$ directed neighbor relaxations. With a binary heap, its bound is
$O(mm!\log(m!))$ plus seeding, target readout, and permutation-indexing
work. Distances need $O(m!)$ entries; edges need not be materialized.
For distinct retained fields, enumerating all dropped-suffix completions
across legal target prefixes visits at most $m!$ states. This is useful
only when that full-origin permutation space fits the budget.

#### Cut search and strata

Finding a minimum-product cut is itself an exponential search in the
worst case. Memoized exhaustive search has at most $2^n$ selected-variable
subsets for $n$ candidate variables, with polynomial graph work per state.
A greedy incumbent and lower bounds reduce practical work but give no
polynomial worst-case guarantee. Apply a graph-search budget separately
from the budget for enumerating layout states.

A coarse alternative fixes every recursive IDB row layout, incurring a
factor $\prod_R \operatorname{arity}(R)!$. It can be much larger than a
cut over a few two-state physical block variables, and may still leave
undirected cycles involving arrangements or reconverging consumers.
Validate the forest condition rather than assuming IDB enumeration is
sufficient.

Per-stratum optimization is exact **conditional on its boundary layouts**.
Freezing imported/exported layouts while solving strata in some order is
a coordination heuristic. For a global guarantee, either solve the graph
including cross-stratum boundary variables or pass exact cost messages
over their joint assignments. A multi-variable boundary can require a
product-sized separator table. Neither reverse stratum order nor a
particular root choice removes this coupling.

Likewise, forcing a group of attributes to stay consecutive is an exact
reduction only if a property of all incident factors and constraints
proves that an optimum with that restriction exists. Merely observing
no explicit precedence between two fields is insufficient. An unproved
contraction is a restricted-domain heuristic and must be reported as such.

### 7. Cutset and DP cost on benchmark plans

The earlier analysis covered **16 query plans, 82 strata, and 55 datasets**
from main at
[`5d345922`](https://github.com/flowlog-rs/flowlog/commit/5d345922654a6ded5a296511a3d07d898a258adb).
Multiple datasets of one query share the same layout problem, so work is
counted once per query. The tables below give both the archived
fixed-orientation counts and a recount allowing join-side swaps on those
same plans.

Both configurations use per-stratum field contraction, full-row
intermediate-layout elimination, additive source-block factorization, and
independent connected components. Each stratum's imported layouts are
free. The free-orientation configuration adds a two-state variable for
each modeled swappable join, shared by all factors depending on that
join. Antijoins retain their required orientation.

These are structural optimization budgets for the reduced per-stratum
model, not runtime measurements or computed minimum values of $I$.
The contraction and boundary assumptions are part of that model; the
counts do not establish an unrestricted cross-stratum optimum.

#### Counting convention

Charge $m_f$ units per factor-table entry. For the residual DP, count state
assignments visited by a selected variable-elimination order:

$$
\begin{aligned}
B_H&=\sum_{f\in H}m_f\prod_{x\in X_f}d_x,\\
P_H&=\prod_{x\in S_H}d_x,\\
T_H^{\mathrm{VE}}&=
\sum_{\text{elimination bags }b}\prod_{x\in b}d_x,\\
W&=\sum_H\left[B_H+P_HT_H^{\mathrm{VE}}\right].
\end{aligned}
$$

Each factor table is budgeted once. For each component, choose the
smallest state budget among three elimination-order heuristics, after
conditioning a minimum-product cut. A separate reconstruction of every
elimination bag verifies the reported totals. All 82 fixed-orientation
rows reproduce the archived analysis exactly.

This preserves the earlier accounting convention. An elimination-bag
assignment differs from a factor-table visit in the forest-message bound
in section 6. Degree-based arithmetic, graph-search operations, scalar
lookups, and outer minimum scans are excluded from this budget. Those
last two counters are retained separately in the CSV.

#### Results by query

Both configurations have **62 strata already forming forests**, and all
cut searches proved the minimum domain product for their fixed factor
graphs. The largest independent-component cut product is **64** with
fixed orientation and **2,048** with free orientation.

The construction and repeated-DP columns below use free orientation;
the fixed-total column reproduces the earlier result. The cut column is
the largest product in an individual connected component, not a product
across unrelated components.

| Query | Datasets | Fixed total | Free construction | Free DP | Free total | Max free cut product |
|---|---:|---:|---:|---:|---:|---:|
| TC | 2 | 32 | 30 | 12 | 42 | 1 |
| SG | 2 | 53 | 54 | 24 | 78 | 1 |
| Reach | 4 | 11 | 11 | 4 | 15 | 1 |
| CC | 3 | 26 | 26 | 10 | 36 | 1 |
| SSSP | 2 | 33 | 27 | 6 | 33 | 1 |
| Bipartite | 4 | 29 | 36 | 12 | 48 | 1 |
| Dyck | 2 | 142 | 148 | 54 | 202 | 1 |
| CRDT | 1 | 9,666 | 7,510 | 5,724 | 13,234 | 4 |
| Galen | 1 | 2,388 | 1,280 | 3,458 | 4,738 | 16 |
| Andersen | 2 | 86 | 90 | 36 | 126 | 1 |
| Polonius | 4 | 3,602 | 2,222 | 4,378 | 6,600 | 16 |
| CSPA | 3 | 178 | 176 | 62 | 238 | 2 |
| CSDA | 3 | 32 | 30 | 12 | 42 | 1 |
| CVC5 | 1 | 45,222,512 | 38,774,858 | 74,081,328 | 112,856,186 | 2,048 |
| Z3 | 1 | 45,222,512 | 38,774,858 | 74,081,328 | 112,856,186 | 2,048 |
| DOOP | 20 | 236,724 | 196,458 | 48,782 | 245,240 | 4 |
| **Total** | **55** | **90,698,026** | **77,757,814** | **148,225,230** | **225,983,044** | **2,048** |

The free-orientation budget is **225,983,044**, versus **90,698,026** with
fixed orientation, or **2.49 times** the earlier budget. Trying two
orientations doubles a local join trial. In this source-block factorization,
the common orientation variable can also enlarge the cut product and
repeated DP work; the complete budget is not obtained by doubling every
entry in the old table. This is the cost of the stated factorization and
cut-enumeration schedule, not a lower bound on every possible solver.

The [per-stratum CSV](attribute-layout-cutset-costs.csv) contains all
**164 stratum/configuration rows**, with `orientation-fixed` and
`orientation-free` modes. `outer_product` is the combined product across
all cut variables of a stratum; `max_component_outer` is the largest
component product. `cached_budget` sums components independently.
State-table sizes count entries, not bytes or process RSS.

#### CVC5 and Z3

These two queries have identical layout-problem structure in this
analysis. For either query, the per-stratum budgets are:

| Stratum | Fixed total | Free construction | Free DP | Free total | Free combined cut product |
|---|---:|---:|---:|---:|---:|
| 0 | 43,275,751 | 36,603,779 | 57,650,932 | 94,254,711 | 12 |
| 1 | 7,612 | 4,628 | 4,240 | 8,868 | 4 |
| 2 | 367,067 | 36,073 | 12,517,378 | 12,553,451 | 2,048 |
| 3 | 20,594 | 16,272 | 6,068 | 22,340 | 2 |
| 4 | 554 | 608 | 102 | 710 | 1 |
| 5 | 3,162 | 2,134 | 1,360 | 3,494 | 4 |
| 6 | 1,547,772 | 2,111,364 | 3,901,248 | 6,012,612 | 32 |

In the free-orientation configuration, stratum 0 contributes about
**83.5%** of each query's total budget. Its largest local factor contains
**2,073,600 states**, despite its cut product being only 12. A small cut
controls outer repetition; it does not remove large local transitions.

Stratum 2 illustrates the orientation-related increase: its cut grows
from six to eleven two-state variables, so the product grows from
$2^6=64$ to $2^{11}=2{,}048$. Its cached budget grows from 367,067 to
12,553,451. This is still much smaller than the outer enumeration of all
five recursive IDB row layouts:

$$
3!\times4!\times3!\times4!\times5!=2{,}488{,}320.
$$

That IDB-only enumeration would also require checking for remaining
arrangement coupling. Selecting a cut over physical layout variables
avoids paying that full product.

DOOP's final stratum has 147 nontrivial variables, including orientation,
and 137 factors. Its cycle rank is 4. A three-variable cut has combined
product 8, with largest independent component product 4; its budget is
11,679. The full DOOP query totals 245,240. This shows why graph size
alone is a poor estimate of cut-enumeration work.

## Implementation proposal

This section proposes an implementation; the preceding sections define
the algorithm independently of compiler data structures.

### Placement and representation

Add a physical-layout planning pass in `flowlog-planner`, after the
existing transformations and actual sharing identities are available and
before `flowlog-codegen` emits tuple accesses. Keep the existing join tree
and operator structure. The initial pass should choose layouts and legal
join orientations without inserting or deleting runtime dedups or
arrangements.

Introduce a proposed `PhysicalLayoutPlan` carrying the selected field
permutations, join orientations, and conversions for each physical
representation. Keep logical attribute identity separate from physical
tuple position. A representation identity must include its scope and
actual sharing identity; a relation name alone cannot identify a trace.
Recursive feedback within a stratum and a completed relation exposed to a
later stratum must remain distinct where the current planner distinguishes
them. See [the scope of canonical sharing](canonical-form.md#scope-the-one-assumption-a-caller-can-break).

Extract a solver model with these records:

- Layout variables with legal permutation domains and any pinned states.
- Join metadata with common key origins, input value origins, and legal
  orientations.
- Sorting factors with stable identities, variable scopes, and composed
  source-to-target origin mappings, including dropped origins for scoring.
- Representation and boundary mappings that let a winning assignment be
  translated back into the existing plan.

Do not identify arrangements solely because their canonical logical rows
agree. Their physical key membership, layout, scope, and runtime sharing
must also be compatible. If applying a layout changes representation
fingerprints, regenerate dependent references consistently or retain a
separate physical identity layer.

### Solver components

Separate graph extraction, cost evaluation, cut selection, forest solving,
and plan application. A practical implementation sequence is:

1. Implement the generic finite-domain factor model and raw-I oracle,
   including projection completion and composed mappings. Retain a direct
   evaluator as the reference for optimized cost calculations.
2. Implement exact forest messages with backpointers. Test every rooting
   direction of a binary join, not only forward transitions.
3. Add singleton substitution, shared-key aliases, independent components,
   and the proven full-row and source-block reductions. Validate that
   their reconstructed assignments reproduce the unreduced objective.
4. Add greedy cut selection and bounded branch and bound, retaining a
   verified feasible cut at all times. Report whether minimum cut product
   was proved independently of whether layout enumeration was exhaustive.
5. Cache local tables subject to explicit entry and byte budgets. Cache
   conditional messages by the cut variables they actually depend on.
   Consider the distance transform only after the generic solver is
   validated and dense transitions are shown to dominate planning work.

Use deterministic state ordering and prefer the existing main layout and
orientation among equal-score solutions. Cost arithmetic must be exact
over supported widths and sums, including negative reference-difference
factors; detect unsupported ranges rather than silently wrapping or
conflating overflow with a legal score.

Initially, a per-stratum deployment can pin already established boundary
layouts and clearly report conditional optimality. A later global mode
can retain boundary variables and their separator messages. Do not label
the former globally optimal or treat a frozen interface as if it had been
minimized jointly with its consumers.

### Applying and validating the result

Have code generation consume the selected physical plan when constructing
arrangement tuples, join key/value accesses, projections, head unions,
and feedback bindings. Rewrite all contributors to a shared head into the
same selected physical schema. Restore required external argument order
at input/output boundaries. Preserve set membership, signed updates,
aggregate semantics, and timestamp handling.

The first implementation should keep opaque computed expressions and
operators without an established layout contract at their existing legal
layout. Aggregate grouping inputs can use the composed projection of
their non-aggregate fields for alignment; aggregate payloads and result
fields need their own origin and ordering contract before they become
free variables.

Validate the optimizer at three levels:

- Compare raw-I formulas with explicit binary-record enumeration, including
  empty and partial projections, renamed fields, repeated references, and
  composed projection chains. Constants do not affect a comparator;
  repeated origins use their first effective comparison position.
- On small complete models, compare forest DP and cut-conditioned DP with
  exhaustive assignments. Include reverse join messages, both join
  orientations, multiple head producers, reconverging branches, separate
  arrangements of one relation, and recursive boundary variables.
- Run the repository's correctness suites, then compare generated physical
  plans and performance against the same main revision. Recompute the
  full unreduced objective from the applied layout, and verify every
  shared key, mapping, and external schema before benchmarking.

Expose planning diagnostics: state-domain sizes, graph components, cut
variables and domain product, cut-search proof status, factor/message
entries, cost-evaluation counts, planning time, and the selected objective.
If a solver budget forces a heuristic fallback, preserve a complete legal
assignment and report the loss of the layout-optimality guarantee.

## Experimental context

The motivating full-suite comparison used main at [44ad2c5d][baseline]
and the rule-head field-order experiment at [d6ae8553][experiment]. It
covered 55 workloads with five fresh runs per version, 32 workers on one
NUMA node, and median execution time. The reported execution interval
included dataflow assembly, input loading, and execution; it excluded
compilation. The Jython numbers above refer to that comparison. The
Scallop and Z3 explanations use earlier profiles and layout experiments.

These observations motivate the assignment problem; they are not
measurements of the current main branch. This description does not add
new benchmark runs or claim that one layout improves every workload.

Related FlowLog documentation:

- [Canonical form and the physical plan](canonical-form.md).
- [Mutability, collection semantics, and deduplication](mutability.md).

[field-order-pr]: https://github.com/flowlog-rs/flowlog/pull/418
[baseline]: https://github.com/flowlog-rs/flowlog/commit/44ad2c5d
[experiment]: https://github.com/altramarine/flowlog/commit/d6ae8553f67861b43b15a697fb0016bb42be26e0
