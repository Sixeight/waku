#!/usr/bin/env python3
"""Measure CLI startup and completion output without invoking Git."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import sys
import time

sys.dont_write_bytecode = True
import benchmark


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", action="append", required=True, metavar="LABEL=PATH")
    parser.add_argument("--repetitions", type=int, default=31)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.repetitions < 1:
        parser.error("repetitions must be positive")
    binaries = benchmark.parse_binaries(parser, args.binary)
    env = dict(os.environ, NO_COLOR="1", TERM="dumb", LC_ALL="C")
    results = {}
    for case, arguments in {"help": ["--help"], "zsh": ["completions", "zsh"],
                            "bash": ["completions", "bash"]}.items():
        samples = {label: [] for label in binaries}
        outputs = {}
        for round_index in range(args.repetitions + benchmark.WARMUPS):
            order = list(binaries)
            offset = round_index % len(order)
            for label in order[offset:] + order[:offset]:
                if outputs.get(label, {}).get("exit_code", 0) != 0:
                    continue
                started = time.perf_counter()
                result = subprocess.run(["git-waku", *arguments], executable=binaries[label],
                                        env=env, capture_output=True)
                elapsed = (time.perf_counter() - started) * 1000
                if round_index >= benchmark.WARMUPS:
                    samples[label].append(elapsed)
                evidence = {"exit_code": result.returncode, "stdout_bytes": len(result.stdout),
                            "stdout_sha256": hashlib.sha256(result.stdout).hexdigest()}
                if label in outputs and outputs[label] != evidence:
                    raise AssertionError(f"unstable output: {case}/{label}")
                outputs[label] = evidence
        if case == "help" and len({entry["stdout_sha256"] for entry in outputs.values()}) != 1:
            raise AssertionError("help output differs")
        results[case] = {
            label: dict(outputs[label], samples_ms=values,
                        median_ms=statistics.median(values) if values else None)
            for label, values in samples.items()
        }
    report = {
        "platform": platform.platform(), "repetitions": args.repetitions,
        "warmups": benchmark.WARMUPS,
        **benchmark.binary_report(binaries),
        "results": results,
    }
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    for case, variants in results.items():
        print(case, {label: data["median_ms"] for label, data in variants.items()})


if __name__ == "__main__":
    main()
