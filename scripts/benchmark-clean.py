#!/usr/bin/env python3
"""Compare clean dry-runs and record Git subprocess counts outside timing."""

import argparse
from collections import Counter
from contextlib import ExitStack
import json
from pathlib import Path
import platform
import resource
import statistics
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True
import benchmark


def stop_agents(processes):
    for process in processes:
        if process.poll() is None:
            process.terminate()
    for process in processes:
        process.wait(timeout=10)


def setup_gone(parent, env, count):
    root = benchmark.setup_origin(parent, env)
    branches = []
    for index in range(count):
        branch = f"gone-{index:03}"
        branches.append(branch)
        worktree = parent / "repo-worktrees" / branch
        benchmark.git(root, env, "worktree", "add", "-q", "-b", branch, str(worktree))
        (worktree / "work.txt").write_text(f"unique content {index}\n")
        benchmark.git(worktree, env, "add", ".")
        benchmark.git(worktree, env, "commit", "-qm", branch)
        benchmark.track_origin(root, env, branch)
    return root, branches


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", action="append", required=True, metavar="LABEL=PATH")
    parser.add_argument("--candidates", nargs="+", type=int, default=[48, 192])
    parser.add_argument("--case", action="append", choices=("mixed", "gone", "conflict"))
    parser.add_argument("--repetitions", type=int, default=7)
    parser.add_argument("--artifact-files", type=int, default=0,
                        help="Untracked configured copy files per worktree")
    parser.add_argument("--agents", type=int, default=0,
                        help="Sleeping agent-named fixture processes in separate directories")
    parser.add_argument("--history", type=int, default=0,
                        help="Advance main after branch creation with this many commits")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if (args.repetitions < 1 or min(args.candidates) < 3 or args.history < 0
            or args.artifact_files < 0 or args.agents < 0):
        parser.error("repetitions must be positive, candidates at least 3, "
                     "and history, artifact files and agents nonnegative")
    binaries = benchmark.parse_binaries(parser, args.binary)
    env = benchmark.isolated_env()
    results = {}
    with tempfile.TemporaryDirectory(prefix="waku-clean-") as temporary, ExitStack() as cleanups:
        for count in args.candidates:
            for case in args.case or ("mixed", "gone"):
                parent = Path(temporary).resolve() / f"{case}-{count}"
                setup = benchmark.setup_clean if case == "mixed" else setup_gone
                root, branches = setup(parent, env, count)
                processes = []
                if args.agents:
                    agent = parent / "codex"
                    agent.symlink_to("/bin/sleep")
                    cleanups.callback(stop_agents, processes)
                    for index in range(args.agents):
                        directory = parent / "agents" / str(index) / "nested" / "workspace"
                        directory.mkdir(parents=True)
                        processes.append(subprocess.Popen([agent, "3600"], cwd=directory))
                if args.artifact_files:
                    benchmark.git(root, env, "config", "waku.copy.include", "generated")
                    for branch in branches:
                        directory = parent / "repo-worktrees" / branch / "generated"
                        directory.mkdir()
                        for index in range(args.artifact_files):
                            (directory / f"file-{index:05}.txt").write_text("generated\n")
                if case == "conflict":
                    (root / "work.txt").write_bytes(b"main\x00content\n")
                    benchmark.git(root, env, "add", "work.txt")
                    benchmark.git(root, env, "commit", "-qm", "conflicting main")
                    benchmark.git(root, env, "push", "-q", "origin", "main")
                if args.history:
                    previous = benchmark.git(root, env, "rev-parse", "HEAD")
                    stream = []
                    for index in range(1, args.history + 1):
                        message = f"history {index}\n"
                        stream.append(
                            f"commit refs/heads/main\nmark :{index}\n"
                            f"committer Benchmark <benchmark@example.invalid> {1767225600 + index} +0000\n"
                            f"data {len(message)}\n{message}from {previous}\n\n"
                        )
                        previous = f":{index}"
                    subprocess.run(["git", "fast-import", "--quiet"], input="".join(stream),
                                   cwd=root, env=env, text=True, capture_output=True, check=True)
                    benchmark.git(root, env, "push", "-q", "origin", "main")
                worktrees = benchmark.git(root, env, "worktree", "list", "--porcelain")
                refs = benchmark.git(root, env, "for-each-ref", "--format=%(refname) %(objectname)")
                expected = None
                samples = {label: [] for label in binaries}
                cpu_samples = {label: [] for label in binaries}

                def verify(result):
                    for branch in branches:
                        if branch not in result.stdout:
                            raise AssertionError(f"missing branch: {branch}")
                    if result.stdout != expected and expected is not None:
                        raise AssertionError("candidate output differs")
                    if benchmark.git(root, env, "worktree", "list", "--porcelain") != worktrees:
                        raise AssertionError("dry-run changed registered worktrees")
                    if benchmark.git(root, env, "for-each-ref", "--format=%(refname) %(objectname)") != refs:
                        raise AssertionError("dry-run changed refs")

                for round_index in range(args.repetitions + benchmark.WARMUPS):
                    for label in benchmark.round_order(binaries, round_index):
                        before = resource.getrusage(resource.RUSAGE_CHILDREN)
                        started = time.perf_counter()
                        result = benchmark.run([binaries[label], "clean", "--dry-run"], root, env)
                        elapsed = (time.perf_counter() - started) * 1000
                        after = resource.getrusage(resource.RUSAGE_CHILDREN)
                        verify(result)
                        if expected is None:
                            expected = result.stdout
                        if round_index >= benchmark.WARMUPS:
                            samples[label].append(elapsed)
                            cpu_samples[label].append(1000 * (
                                after.ru_utime + after.ru_stime - before.ru_utime - before.ru_stime
                            ))
                profiles = {}
                for index, (label, binary) in enumerate(binaries.items()):
                    trace = parent / f"trace-{index}.json"
                    result = benchmark.run([binary, "clean", "--dry-run"], root,
                                           dict(env, GIT_TRACE2_EVENT=str(trace)))
                    verify(result)
                    starts = [event["argv"] for line in trace.read_text().splitlines()
                              if (event := json.loads(line)).get("event") == "start"]
                    profiles[label] = {
                        "git_processes": len(starts),
                        "unique_count_processes": sum("rev-list" in argv and "--count" in argv
                                                      for argv in starts),
                        "batch_count_processes": sum(any("ahead-behind:" in arg for arg in argv)
                                                     for argv in starts),
                        "single_merge_processes": sum("merge-tree" in argv and "--stdin" not in argv
                                                      for argv in starts),
                        "batch_merge_processes": sum("merge-tree" in argv and "--stdin" in argv
                                                     for argv in starts),
                        "command_counts": dict(sorted(Counter(
                            argv[1] if len(argv) > 1 else "(no subcommand)" for argv in starts
                        ).items())),
                    }
                name = f"{case}_{count}"
                results[name] = {
                    label: {
                        **benchmark.summarize(values),
                        "median_cpu_ms": round(statistics.median(cpu_samples[label]), 3),
                        "cpu_samples_ms": [round(value, 3) for value in cpu_samples[label]],
                        "profile": profiles[label],
                    } for label, values in samples.items()
                }
                print(name + ": " + ", ".join(
                    f"{label}={data['median_ms']:.3f} ms "
                    f"({data['profile']['unique_count_processes']} individual + "
                    f"{data['profile']['batch_count_processes']} batch counts)"
                    for label, data in results[name].items()
                ), flush=True)
                stop_agents(processes)
    report = {
        "repetitions": args.repetitions, "warmups": benchmark.WARMUPS,
        "history": args.history,
        "artifact_files_per_worktree": args.artifact_files,
        "fixture_agents": args.agents,
        "platform": platform.platform(),
        "git_version": benchmark.run(["git", "--version"], Path.cwd(), env).stdout.strip(),
        **benchmark.binary_report(binaries),
        "results": results,
        "behavior_checks": "candidate output, registered worktrees and refs checked every run",
    }
    args.output.write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
