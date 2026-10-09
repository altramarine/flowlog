//! Set normalization that preserves key-value grouping.
//!
//! Total clocks compare pair counts against an input trace. Recursive
//! products retain minimal presence times or use signed reduction.

use differential_dataflow::AsCollection;
use differential_dataflow::ExchangeData;
use differential_dataflow::VecCollection;
use differential_dataflow::difference::Present;
use differential_dataflow::difference::Semigroup;
use differential_dataflow::hashable::Hashable;
use differential_dataflow::lattice::Lattice;
use differential_dataflow::trace::BatchReader;
use differential_dataflow::trace::Cursor;
use differential_dataflow::trace::Navigable;
use differential_dataflow::trace::TraceReader;
use differential_dataflow::trace::implementations::ValBuilder;
use differential_dataflow::trace::implementations::ValSpine;
use timely::PartialOrder;
use timely::dataflow::channels::pact::Pipeline;
use timely::dataflow::operators::generic::Operator;
use timely::order::Product;
use timely::order::TotalOrder;
use timely::progress::Timestamp;

use super::dedup::Epoch;

/// Normalizes membership of complete `(key, value)` pairs. Exchange hashes
/// only the key; different values under that key remain separate members.
/// Counts, deletions, and recursive timestamps follow `flowlog_dedup`.
pub fn flowlog_dedup_by_key<C: FlowlogDedupByKey>(collection: C) -> C {
    collection.dedup_by_key()
}

/// Compile-time dispatch for key-value set normalization.
pub trait FlowlogDedupByKey: Sized {
    /// Preserves the pair layout while normalizing complete-pair membership.
    fn dedup_by_key(self) -> Self;
}

impl<'scope, K, V> FlowlogDedupByKey for VecCollection<'scope, (), (K, V), Present>
where
    K: ExchangeData + Hashable,
    V: ExchangeData,
{
    fn dedup_by_key(self) -> Self {
        self.arrange_by_key_named("Arrange: DedupByKey")
            .as_collection(|key, value| (key.clone(), value.clone()))
    }
}

impl<'scope, E: Epoch, K, V> FlowlogDedupByKey for VecCollection<'scope, E, (K, V), Present>
where
    K: ExchangeData + Hashable,
    V: ExchangeData,
{
    fn dedup_by_key(self) -> Self {
        threshold_by_key_total(self, |_, prior| prior.is_none().then_some(Present))
    }
}

impl<'scope, I: Epoch, K, V> FlowlogDedupByKey
    for VecCollection<'scope, Product<(), I>, (K, V), Present>
where
    K: ExchangeData + Hashable,
    V: ExchangeData,
{
    fn dedup_by_key(self) -> Self {
        threshold_by_key_total(self, |_, prior| prior.is_none().then_some(Present))
    }
}

impl<'scope, E: Epoch, I: Epoch, K, V> FlowlogDedupByKey
    for VecCollection<'scope, Product<E, I>, (K, V), Present>
where
    K: ExchangeData + Hashable,
    V: ExchangeData,
{
    fn dedup_by_key(self) -> Self {
        first_pairs(self)
    }
}

impl<'scope, K, V> FlowlogDedupByKey for VecCollection<'scope, (), (K, V), i32>
where
    K: ExchangeData + Hashable,
    V: ExchangeData,
{
    fn dedup_by_key(self) -> Self {
        threshold_by_key_total(self, membership_delta)
    }
}

impl<'scope, E: Epoch, K, V> FlowlogDedupByKey for VecCollection<'scope, E, (K, V), i32>
where
    K: ExchangeData + Hashable,
    V: ExchangeData,
{
    fn dedup_by_key(self) -> Self {
        threshold_by_key_total(self, membership_delta)
    }
}

impl<'scope, I: Epoch, K, V> FlowlogDedupByKey
    for VecCollection<'scope, Product<(), I>, (K, V), i32>
where
    K: ExchangeData + Hashable,
    V: ExchangeData,
{
    fn dedup_by_key(self) -> Self {
        threshold_by_key_total(self, membership_delta)
    }
}

impl<'scope, E: Epoch, I: Epoch, K, V> FlowlogDedupByKey
    for VecCollection<'scope, Product<E, I>, (K, V), i32>
where
    K: ExchangeData + Hashable,
    V: ExchangeData,
{
    fn dedup_by_key(self) -> Self {
        self.arrange_by_key_named("Arrange: DedupByKey")
            .reduce_abelian::<_, ValBuilder<K, V, Product<E, I>, i32>, ValSpine<K, V, Product<E, I>, i32>, _, _>(
                "DedupByKey",
                |_, input, output| {
                    output.extend(
                        input.iter().filter(|(_, count)| *count > 0)
                            .map(|(value, _)| ((*value).clone(), 1)),
                    );
                },
                |updates, key, changes| {
                    updates.clear();
                    updates.extend(changes.drain(..).map(|(value, time, diff)| {
                        ((key.clone(), value), time, diff)
                    }));
                },
            )
            .as_collection(|key, value| (key.clone(), value.clone()))
    }
}

fn membership_delta(new: &i32, prior: Option<&i32>) -> Option<i32> {
    let delta = i32::from(*new > 0) - i32::from(prior.is_some_and(|count| *count > 0));
    (delta != 0).then_some(delta)
}

/// Evaluates each pair's old and new weights in total timestamp order.
/// The input trace retains raw counts even when output membership is one.
fn threshold_by_key_total<'scope, T, K, V, R, F>(
    collection: VecCollection<'scope, T, (K, V), R>,
    mut threshold: F,
) -> VecCollection<'scope, T, (K, V), R>
where
    T: Timestamp + Lattice + TotalOrder,
    K: ExchangeData + Hashable,
    V: ExchangeData,
    R: ExchangeData + Semigroup,
    F: FnMut(&R, Option<&R>) -> Option<R> + 'static,
{
    let arranged = collection.arrange_by_key_named("Arrange: DedupByKey");
    let mut trace = arranged.trace;
    arranged
        .stream
        .unary(Pipeline, "DedupByKey", move |_, _| {
            let mut times = Vec::new();
            move |input, output| {
                input.for_each(|capability, batches| {
                    let mut session = output.session(&capability);
                    for batch in batches.drain(..) {
                        let mut history = Vec::new();
                        trace.map_batches(|old| {
                            if PartialOrder::less_equal(old.upper(), batch.lower()) {
                                history.push(std::rc::Rc::clone(old));
                            }
                        });
                        // Probe batches separately: a merged cursor's seek_val
                        // may reach a constituent whose key cursor is exhausted.
                        let mut prior: Vec<_> = history.iter().map(|old| old.cursor()).collect();
                        let mut current = batch.cursor();
                        while let Some(key) = current.get_key(&batch) {
                            for (cursor, old) in prior.iter_mut().zip(&history) {
                                cursor.seek_key(old, key);
                            }
                            while let Some(value) = current.get_val(&batch) {
                                let mut count: Option<R> = None;
                                for (cursor, old) in prior.iter_mut().zip(&history) {
                                    if cursor.get_key(old) == Some(key) {
                                        cursor.seek_val(old, value);
                                        if cursor.get_val(old) == Some(value) {
                                            cursor.map_times(old, |_, diff| {
                                                if let Some(count) = &mut count {
                                                    count.plus_equals(diff);
                                                } else {
                                                    count = Some(diff.clone());
                                                }
                                            });
                                        }
                                    }
                                }
                                current.map_times(&batch, |time, diff| {
                                    times.push((time.clone(), diff.clone()));
                                });
                                times.sort_unstable_by(|left, right| left.0.cmp(&right.0));
                                for (time, diff) in times.drain(..) {
                                    let mut new = count.clone().unwrap_or_else(|| diff.clone());
                                    if count.is_some() {
                                        new.plus_equals(&diff);
                                    }
                                    if let Some(delta) = threshold(&new, count.as_ref())
                                        && !delta.is_zero()
                                    {
                                        session.give(((key.clone(), value.clone()), time, delta));
                                    }
                                    count = Some(new);
                                }
                                current.step_val(&batch);
                            }
                            current.step_key(&batch);
                        }
                        // Keep old and incoming batches separable until this
                        // batch's history comparison has finished.
                        trace.set_logical_compaction(batch.upper().borrow());
                        trace.set_physical_compaction(batch.upper().borrow());
                    }
                });
            }
        })
        .as_collection()
}

fn first_pairs<'scope, E: Epoch, I: Epoch, K, V>(
    collection: VecCollection<'scope, Product<E, I>, (K, V), Present>,
) -> VecCollection<'scope, Product<E, I>, (K, V), Present>
where
    K: ExchangeData + Hashable,
    V: ExchangeData,
{
    let arranged = collection.arrange_by_key_named("Arrange: DedupByKey");
    let mut trace = arranged.trace;
    arranged
        .stream
        .unary(Pipeline, "DedupByKey", move |_, _| {
            let mut times = Vec::new();
            move |input, output| {
                input.for_each(|capability, batches| {
                    let mut session = output.session(&capability);
                    for batch in batches.drain(..) {
                        let mut history = Vec::new();
                        trace.map_batches(|old| {
                            if PartialOrder::less_equal(old.upper(), batch.lower()) {
                                history.push(std::rc::Rc::clone(old));
                            }
                        });
                        let mut prior: Vec<_> = history.iter().map(|old| old.cursor()).collect();
                        let mut current = batch.cursor();
                        while let Some(key) = current.get_key(&batch) {
                            for (cursor, old) in prior.iter_mut().zip(&history) {
                                cursor.seek_key(old, key);
                            }
                            while let Some(value) = current.get_val(&batch) {
                                for (cursor, old) in prior.iter_mut().zip(&history) {
                                    if cursor.get_key(old) == Some(key) {
                                        cursor.seek_val(old, value);
                                        if cursor.get_val(old) == Some(value) {
                                            cursor.map_times(old, |time, _| {
                                                times.push((time.clone(), false));
                                            });
                                        }
                                    }
                                }
                                current.map_times(&batch, |time, _| {
                                    times.push((time.clone(), true));
                                });
                                times.sort_unstable();
                                let mut least_inner: Option<I> = None;
                                for (time, incoming) in times.drain(..) {
                                    if least_inner.as_ref().is_none_or(|least| time.inner < *least)
                                    {
                                        if incoming {
                                            session.give((
                                                (key.clone(), value.clone()),
                                                time.clone(),
                                                Present,
                                            ));
                                        }
                                        least_inner = Some(time.inner);
                                    }
                                }
                                current.step_val(&batch);
                            }
                            current.step_key(&batch);
                        }
                        trace.set_logical_compaction(batch.upper().borrow());
                        trace.set_physical_compaction(batch.upper().borrow());
                    }
                });
            }
        })
        .as_collection()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use differential_dataflow::input::Input;
    use timely::dataflow::operators::probe::Handle;

    use super::*;

    type Pair = (u64, char);
    type PairUpdate<R> = (Pair, u32, R);

    fn run_epochs<R>(epochs: Vec<Vec<(Pair, R)>>) -> Vec<PairUpdate<R>>
    where
        R: ExchangeData + Semigroup + Sync,
        for<'scope> VecCollection<'scope, u32, Pair, R>: FlowlogDedupByKey,
    {
        let guards = timely::execute(timely::Config::process(4), move |worker| {
            let seen = Rc::new(RefCell::new(Vec::new()));
            let probe = Handle::new();
            let mut input = worker.dataflow::<u32, _, _>(|scope| {
                let (input, rows) = scope.new_collection::<Pair, R>();
                let seen = Rc::clone(&seen);
                flowlog_dedup_by_key(rows)
                    .inspect(move |update| seen.borrow_mut().push(update.clone()))
                    .probe_with(&probe);
                input
            });
            for (epoch, updates) in epochs.iter().enumerate() {
                for (index, (pair, diff)) in updates.iter().enumerate() {
                    if index % worker.peers() == worker.index() {
                        input.update(*pair, diff.clone());
                    }
                }
                input.advance_to(epoch as u32 + 1);
                input.flush();
                while probe.less_than(input.time()) {
                    worker.step();
                }
            }
            input.close();
            while worker.step() {}
            seen.take()
        })
        .unwrap();
        guards.join().into_iter().flat_map(Result::unwrap).collect()
    }

    #[test]
    fn presence_retains_distinct_values_and_suppresses_later_pairs_across_workers() {
        let mut actual = run_epochs(vec![
            vec![
                ((1, 'a'), Present),
                ((1, 'a'), Present),
                ((2, 'a'), Present),
            ],
            vec![
                ((1, 'a'), Present),
                ((1, 'b'), Present),
                ((2, 'a'), Present),
            ],
            vec![((1, 'b'), Present), ((1, 'c'), Present)],
        ]);
        actual.sort();
        assert_eq!(
            actual,
            vec![
                ((1, 'a'), 0, Present),
                ((1, 'b'), 1, Present),
                ((1, 'c'), 2, Present),
                ((2, 'a'), 0, Present),
            ]
        );
    }

    #[test]
    fn signed_pairs_keep_witness_counts_through_deletion_and_reinsertion() {
        let mut actual = run_epochs(vec![
            vec![((1, 'a'), 1), ((1, 'a'), 1), ((1, 'b'), 1), ((2, 'a'), -1)],
            vec![((1, 'a'), -1), ((1, 'b'), -1), ((2, 'a'), 1)],
            vec![((1, 'a'), -1), ((2, 'a'), 1)],
            vec![((1, 'a'), 1), ((2, 'a'), -2)],
        ]);
        actual.sort();
        assert_eq!(
            actual,
            vec![
                ((1, 'a'), 0, 1),
                ((1, 'a'), 2, -1),
                ((1, 'a'), 3, 1),
                ((1, 'b'), 0, 1),
                ((1, 'b'), 1, -1),
                ((2, 'a'), 2, 1),
                ((2, 'a'), 3, -1),
            ]
        );
    }

    #[test]
    fn batch_signed_pairs_cancel_before_membership_is_tested() {
        let actual = timely::execute_directly(|worker| {
            let seen = Rc::new(RefCell::new(Vec::new()));
            let mut input = worker.dataflow::<(), _, _>(|scope| {
                let (input, rows) = scope.new_collection::<Pair, i32>();
                let seen = Rc::clone(&seen);
                flowlog_dedup_by_key(rows).inspect(move |update| seen.borrow_mut().push(*update));
                input
            });
            for diff in [2, -1, -1] {
                input.update((1, 'a'), diff);
                input.flush();
                worker.step();
            }
            input.update((1, 'b'), 3);
            input.close();
            while worker.step() {}
            seen.take()
        });
        assert_eq!(actual, vec![((1, 'b'), (), 1)]);
    }

    #[test]
    fn recursive_presence_suppresses_a_pair_across_iterations() {
        let mut actual = timely::execute_directly(|worker| {
            let seen = Rc::new(RefCell::new(Vec::new()));
            let probe = Handle::new();
            let mut input = worker.dataflow::<Product<(), u16>, _, _>(|scope| {
                let (input, rows) = scope.new_collection::<Pair, Present>();
                let seen = Rc::clone(&seen);
                flowlog_dedup_by_key(rows)
                    .inspect(move |update| seen.borrow_mut().push(*update))
                    .probe_with(&probe);
                input
            });
            for iteration in 0..3 {
                input.update((1, 'a'), Present);
                input.update((1, char::from(b'b' + iteration)), Present);
                input.advance_to(Product::new((), u16::from(iteration) + 1));
                input.flush();
                while probe.less_than(input.time()) {
                    worker.step();
                }
            }
            input.close();
            while worker.step() {}
            seen.take()
        });
        actual.sort();
        assert_eq!(
            actual,
            vec![
                ((1, 'a'), Product::new((), 0), Present),
                ((1, 'b'), Product::new((), 0), Present),
                ((1, 'c'), Product::new((), 1), Present),
                ((1, 'd'), Product::new((), 2), Present),
            ]
        );
    }

    #[test]
    fn partial_presence_keeps_incomparable_pair_times() {
        let mut actual = timely::execute_directly(|worker| {
            let seen = Rc::new(RefCell::new(Vec::new()));
            let mut input = worker.dataflow::<u32, _, _>(|scope| {
                let (input, rows) = scope.new_collection::<(Pair, u16), Present>();
                let seen = Rc::clone(&seen);
                scope.iterative::<u16, _, _>(|inner| {
                    let rows = rows
                        .enter_at(inner, |(_, iteration)| *iteration)
                        .map(|(pair, _)| pair);
                    flowlog_dedup_by_key(rows)
                        .inspect(move |update| seen.borrow_mut().push(*update))
                        .leave(scope)
                });
                input
            });
            input.update(((1, 'a'), 5), Present);
            input.advance_to(1);
            input.update(((1, 'a'), 1), Present);
            input.update(((1, 'a'), 5), Present);
            input.update(((1, 'b'), 2), Present);
            input.close();
            while worker.step() {}
            seen.take()
        });
        actual.sort();
        assert_eq!(
            actual,
            vec![
                ((1, 'a'), Product::new(0, 5), Present),
                ((1, 'a'), Product::new(1, 1), Present),
                ((1, 'b'), Product::new(1, 2), Present),
            ]
        );
    }

    #[test]
    fn partial_signed_pairs_correct_overlapping_derivations() {
        let mut actual = timely::execute_directly(|worker| {
            let seen = Rc::new(RefCell::new(Vec::new()));
            let mut input = worker.dataflow::<u32, _, _>(|scope| {
                let (input, rows) = scope.new_collection::<(Pair, u16), i32>();
                let seen = Rc::clone(&seen);
                scope.iterative::<u16, _, _>(|inner| {
                    let rows = rows
                        .enter_at(inner, |(_, iteration)| *iteration)
                        .map(|(pair, _)| pair);
                    flowlog_dedup_by_key(rows)
                        .inspect(move |update| seen.borrow_mut().push(*update))
                        .leave(scope)
                });
                input
            });
            input.update(((1, 'a'), 5), 1);
            input.advance_to(1);
            input.update(((1, 'a'), 1), 1);
            input.update(((1, 'b'), 2), 1);
            input.close();
            while worker.step() {}
            seen.take()
        });
        actual.sort();
        assert_eq!(
            actual,
            vec![
                ((1, 'a'), Product::new(0, 5), 1),
                ((1, 'a'), Product::new(1, 1), 1),
                ((1, 'a'), Product::new(1, 5), -1),
                ((1, 'b'), Product::new(1, 2), 1),
            ]
        );
    }
}
