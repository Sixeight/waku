#!/usr/bin/env python3
"""Compare copy workloads using real Git worktrees and verify every checkout."""

import argparse
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True
import benchmark


def manifest(root, env):
    paths = []
    for directory, directories, files in os.walk(root):
        if Path(directory) == root:
            directories[:] = [name for name in directories if name != ".git"]
            files = [name for name in files if name != ".git"]
        paths.extend(str((Path(directory) / name).relative_to(root)) for name in files)
    paths.sort()
    result = subprocess.run(
        ["git", "hash-object", "--stdin-paths", "--no-filters"],
        input="".join(f"{path}\n" for path in paths),
        cwd=root, env=env, capture_output=True, text=True, check=True,
    )
    return list(zip(paths, result.stdout.splitlines(), strict=True))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", action="append", required=True, metavar="LABEL=PATH")
    parser.add_argument("--files", nargs="+", type=int, default=[640, 4096])
    parser.add_argument("--file-bytes", type=int, default=256)
    parser.add_argument("--groups", type=int, default=16)
    parser.add_argument("--mode", action="append", choices=("include", "directory"))
    parser.add_argument("--selective", action="store_true",
                        help="Keep a non-matching ignored file in each directory")
    parser.add_argument("--repetitions", type=int, default=7)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    modes = [mode for mode in ("include", "directory")
             if not args.mode or mode in args.mode]
    if min(*args.files, args.file_bytes, args.groups, args.repetitions) < 1:
        parser.error("counts and file size must be positive")
    binaries = benchmark.parse_binaries(parser, args.binary)
    env = benchmark.isolated_env()
    results = {}
    with tempfile.TemporaryDirectory(prefix="waku-copy-") as temporary:
        for count in args.files:
            root = benchmark.setup_repo(Path(temporary) / str(count), env)
            (root / ".gitignore").write_text("cache/\n")
            (root / ".worktreeinclude").write_text("cache/**/*.dat\n")
            benchmark.git(root, env, "add", ".")
            benchmark.git(root, env, "commit", "-qm", "copy fixture")
            for index in range(count):
                path = root / "cache" / f"group-{index % args.groups:02}" / f"{index}.dat"
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(bytes([index % 251]) * args.file_bytes)
            if args.selective:
                for group in range(min(count, args.groups)):
                    path = root / "cache" / f"group-{group:02}" / "excluded.bin"
                    path.write_bytes(b"not selected by the include pattern\n")
                    marker = path.parent / ".keep"
                    marker.write_text("tracked directory\n")
                    benchmark.git(root, env, "add", "-f", str(marker))
                benchmark.git(root, env, "commit", "-qm", "tracked copy directories")
            expected = manifest(root, env)
            worktree = root.parent / "repo-worktrees" / "bench"
            for mode in modes:
                selected = [entry for entry in expected
                            if mode == "directory" or not entry[0].endswith("/excluded.bin")]
                benchmark.git(root, env, "config", "waku.worktreeinclude",
                              "copy" if mode == "include" else "ignore")
                if mode == "directory":
                    benchmark.git(root, env, "config", "waku.copy.include", "cache")
                samples = {label: [] for label in binaries}
                stages = {label: {} for label in binaries}
                for round_index in range(args.repetitions + benchmark.WARMUPS):
                    for label in benchmark.round_order(binaries, round_index):
                        started = time.perf_counter()
                        result = benchmark.run([binaries[label], "create", "bench"], root, env)
                        elapsed = (time.perf_counter() - started) * 1000
                        if manifest(worktree, env) != selected:
                            raise AssertionError(f"copy mismatch: {mode}, {count}, {label}")
                        if benchmark.git(worktree, env, "status", "--porcelain"):
                            raise AssertionError("worktree is dirty")
                        if benchmark.git(worktree, env, "branch", "--show-current") != "bench":
                            raise AssertionError("wrong branch")
                        if round_index >= benchmark.WARMUPS:
                            samples[label].append(elapsed)
                            for line in result.stderr.splitlines():
                                if line.startswith("PROBE "):
                                    key, value = line[6:].split("=", 1)
                                    stages[label].setdefault(key, []).append(int(value))
                        benchmark.git(root, env, "worktree", "remove", "--force", str(worktree))
                        benchmark.git(root, env, "branch", "-D", "bench")
                name = f"{mode}_{count}"
                results[name] = {
                    label: {**benchmark.summarize(values), "stages": stages[label]}
                    for label, values in samples.items()
                }
                print(name + ": " + ", ".join(
                    f"{label}={value['median_ms']:.3f} ms"
                    for label, value in results[name].items()
                ), flush=True)
    report = {
        "repetitions": args.repetitions, "warmups": benchmark.WARMUPS,
        "files": args.files, "file_bytes": args.file_bytes, "groups": args.groups,
        "selective": args.selective,
        "modes": modes,
        "platform": platform.platform(),
        "git_version": benchmark.run(["git", "--version"], Path.cwd(), env).stdout.strip(),
        "binaries": {label: str(binary) for label, binary in binaries.items()},
        **benchmark.binary_report(binaries),
        "results": results,
        "behavior_checks": "file paths, content hashes, branch and status verified on every run",
    }
    args.output.write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
