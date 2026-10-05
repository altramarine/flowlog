#!/usr/bin/env python3
import argparse
import random
from pathlib import Path


def edges(path: Path):
    with path.open("r", encoding="utf-8", newline="") as source:
        for line_number, line in enumerate(source, start=1):
            fields = line.rstrip("\r\n").split(",")
            if len(fields) != 2:
                raise ValueError(f"{path}:{line_number}: expected two CSV columns")
            yield fields[0], fields[1]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("input", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--fraction", type=float, default=0.70)
    parser.add_argument("--seed", type=int, default=0)
    args = parser.parse_args()

    if not 0.0 < args.fraction <= 1.0:
        parser.error("--fraction must be in (0, 1]")

    vertices = set()
    for source, target in edges(args.input):
        vertices.add(source)
        vertices.add(target)

    ordered_vertices = sorted(vertices)
    count = round(len(ordered_vertices) * args.fraction)
    selected = set(random.Random(args.seed).sample(ordered_vertices, count))

    args.output.parent.mkdir(parents=True, exist_ok=True)
    retained = 0
    with args.output.open("w", encoding="utf-8", newline="") as destination:
        for source, target in edges(args.input):
            if source in selected and target in selected:
                destination.write(f"{source},{target}\n")
                retained += 1

    print(f"vertices={len(ordered_vertices)} selected={count} edges={retained}")


if __name__ == "__main__":
    main()
