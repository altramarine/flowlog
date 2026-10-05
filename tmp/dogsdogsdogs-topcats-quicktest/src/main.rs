use std::cell::Cell;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::rc::Rc;

use differential_dataflow::input::Input;
use differential_dataflow::{AsCollection, Collection};
use differential_dogs3::{CollectionIndex, PrefixExtender};
use timely::dataflow::operators::probe::Handle;
use timely::dataflow::operators::{Inspect, Probe, ToStream, vec::Partition};

fn load_edges(
    filename: &str,
    peers: usize,
    worker_index: usize,
    max_edges: usize,
) -> io::Result<Vec<((u32, u32), usize, isize)>> {
    let reader = BufReader::new(File::open(filename)?);
    let mut edges = Vec::new();

    for (line_index, line) in reader.lines().enumerate() {
        if max_edges != 0 && line_index >= max_edges {
            break;
        }
        if line_index % peers != worker_index {
            continue;
        }

        let line = line?;
        let (src, dst) = line.split_once(',').ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("line {} is not a comma-separated edge", line_index + 1),
            )
        })?;
        let src = src.trim().parse::<u32>().map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid source at line {}: {error}", line_index + 1),
            )
        })?;
        let dst = dst.trim().parse::<u32>().map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid destination at line {}: {error}", line_index + 1),
            )
        })?;
        edges.push(((src, dst), 0, 1));
    }

    Ok(edges)
}

fn main() {
    let filename = std::env::args().nth(1).expect("missing Topcats CSV path");
    let batching = std::env::args()
        .nth(2)
        .expect("missing batch size")
        .parse::<usize>()
        .expect("batch size must be an integer");
    let max_edges = std::env::args()
        .nth(3)
        .expect("missing maximum edge count")
        .parse::<usize>()
        .expect("maximum edge count must be an integer");

    assert!(batching > 0, "batch size must be positive");

    timely::execute_from_args(std::env::args().skip(3), move |worker| {
        let peers = worker.peers();
        let worker_index = worker.index();
        let edges = load_edges(&filename, peers, worker_index, max_edges)
            .unwrap_or_else(|error| panic!("failed to read {filename}: {error}"));
        let edge_count = edges.len();
        let edges2 = edges.clone();

        let relation = worker.dataflow::<usize, _, _>(|scope| {
            CollectionIndex::index(Collection::new(edges.to_stream(scope)))
        });

        let mut index_xz = relation.extend_using(|&(x, _y)| x);
        let mut index_yz = relation.extend_using(|&(_x, y)| y);
        let emitted = Rc::new(Cell::new(0_usize));
        let emitted_inner = Rc::clone(&emitted);
        let mut probe = Handle::new();

        let mut input = worker.dataflow::<usize, _, _>(|scope| {
            let (input, edges) = scope.new_collection();
            let counts = edges.map(|prefix| (prefix, usize::MAX, usize::MAX));
            let counts = index_xz.count(counts, 0);
            let counts = index_yz.count(counts, 1);
            let parts = counts
                .inner
                .partition(2, |((prefix, _count, index), time, diff)| {
                    (index as u64, (prefix, time, diff))
                });
            let proposed_xz = index_xz.propose(parts[0].clone().as_collection());
            let proposed_yz = index_yz.propose(parts[1].clone().as_collection());
            let validated_xz = index_yz.validate(proposed_xz);
            let validated_yz = index_xz.validate(proposed_yz);

            validated_xz
                .concat(validated_yz)
                .inspect(move |_| emitted_inner.set(emitted_inner.get() + 1))
                .probe_with(&mut probe);

            input
        });

        let mut offset = 0;
        while offset < edges2.len() {
            let limit = batching.min(edges2.len() - offset);
            for edge_offset in 0..limit {
                input.insert(edges2[offset + edge_offset].0);
                input.advance_to(offset + edge_offset + 1);
            }
            offset += limit;
            input.flush();
            while probe.less_than(input.time()) {
                worker.step();
            }
        }

        println!(
            "worker {worker_index}: loaded {edge_count} R edges, emitted {} dogdogdog updates",
            emitted.get()
        );
    })
    .expect("timely execution failed");
}
