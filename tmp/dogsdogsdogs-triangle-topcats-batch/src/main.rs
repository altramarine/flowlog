use std::cell::Cell;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use differential_dataflow::input::Input;
use differential_dataflow::AsCollection;
use differential_dogs3::{CollectionIndex, PrefixExtender};
use mimalloc::MiMalloc;
use timely::dataflow::operators::vec::Partition;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

fn load_edges<F>(filename: &str, peers: usize, worker_index: usize, mut on_edge: F) -> io::Result<usize>
where
    F: FnMut((i32, i32)),
{
    let mut file = File::open(filename)?;
    let length = file.metadata()?.len();
    let chunk = length / peers as u64;
    let start = chunk * worker_index as u64;
    let end = if worker_index + 1 == peers {
        length
    } else {
        chunk * (worker_index + 1) as u64
    };
    if start >= end {
        return Ok(0);
    }

    let (mut reader, mut remaining) = if start == 0 {
        (BufReader::new(file), end)
    } else {
        file.seek(SeekFrom::Start(start - 1))?;
        let mut reader = BufReader::new(file);
        let mut before = [0_u8; 1];
        reader.read_exact(&mut before)?;
        let skipped = if before[0] == b'\n' {
            0
        } else {
            reader.skip_until(b'\n')? as u64
        };
        (reader, (end - start).saturating_sub(skipped))
    };

    let mut edge_count = 0;
    let mut line = String::new();
    while remaining > 0 {
        line.clear();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            break;
        }
        remaining = remaining.saturating_sub(read as u64);
        let (src, dst) = line.trim().split_once(',').ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "expected a comma-separated edge")
        })?;
        let src = src.trim().parse::<i32>().map_err(|error| {
            io::Error::new(io::ErrorKind::InvalidData, format!("invalid source: {error}"))
        })?;
        let dst = dst.trim().parse::<i32>().map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid destination: {error}"),
            )
        })?;
        on_edge((src, dst));
        edge_count += 1;
    }

    Ok(edge_count)
}

fn main() {
    let filename = std::env::args().nth(1).expect("missing Topcats CSV path");
    let started = Instant::now();
    let worker_counts = Arc::new(Mutex::new(Vec::new()));
    let worker_counts_inner = Arc::clone(&worker_counts);

    timely::execute_from_args(std::env::args().skip(1), move |worker| {
        let peers = worker.peers();
        let worker_index = worker.index();
        let result_count = Rc::new(Cell::new(0_isize));
        let result_count_inner = Rc::clone(&result_count);

        let mut input = worker.dataflow::<(), _, _>(|scope| {
            let (input, relation) = scope.new_collection::<(i32, i32), isize>();
            let relation = relation.consolidate();
            let forward = CollectionIndex::index(relation.clone());
            let reverse = CollectionIndex::index(relation.clone().map(|(src, dst)| (dst, src)));
            let mut outgoing = forward.extend_using(|&(_a, b)| b);
            let mut incoming = reverse.extend_using(|&(a, _b)| a);
            let counts = relation.map(|prefix| (prefix, usize::MAX, usize::MAX));
            let counts = outgoing.count(counts, 0);
            let counts = incoming.count(counts, 1);
            let partitions = counts.inner.partition(2, |((prefix, _count, index), time, diff)| {
                (index as u64, (prefix, time, diff))
            });
            let proposed_outgoing = outgoing.propose(partitions[0].clone().as_collection());
            let proposed_incoming = incoming.propose(partitions[1].clone().as_collection());
            let validated_outgoing = incoming.validate(proposed_outgoing);
            let validated_incoming = outgoing.validate(proposed_incoming);

            validated_outgoing
                .concat(validated_incoming)
                .map(|((a, b), c)| (a, b, c))
                .inspect(move |(_triangle, _time, diff)| {
                    result_count_inner.set(result_count_inner.get() + *diff);
                });

            input
        });

        let edge_count = load_edges(&filename, peers, worker_index, |edge| input.insert(edge))
            .unwrap_or_else(|error| panic!("failed to read {filename}: {error}"));
        input.close();
        while worker.step() {}

        let mut counts = worker_counts_inner
            .lock()
            .expect("worker result counts are available");
        if counts.len() <= worker_index {
            counts.resize(worker_index + 1, 0);
        }
        counts[worker_index] = result_count.get();
        if worker_index == 0 {
            println!("worker 0: loaded {edge_count} R edges");
        }
    })
    .expect("timely execution failed");

    let triangles = worker_counts
        .lock()
        .expect("worker result counts are available")
        .iter()
        .sum::<isize>();
    println!("triangles: {triangles}");
    println!("total elapsed: {:?}", started.elapsed());
}
