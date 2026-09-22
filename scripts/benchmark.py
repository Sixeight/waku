#!/usr/bin/env python3
"""Compare waku binaries in isolated local Git repositories.

Example:
    python3 scripts/benchmark.py --binary before=/tmp/waku-before \
        --binary after=target/release/git-waku --output /tmp/waku-benchmark.json

Only command execution is timed. Fixture creation and correctness checks are
excluded. Git configuration is isolated and all remotes use the local filesystem.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import tempfile
import time


def run(args, cwd, env):
    result = subprocess.run(
        [str(arg) for arg in args], cwd=cwd, env=env,
        capture_output=True, text=True, check=False,
    )
    if result.returncode:
        raise RuntimeError(
            f"{args!r} failed ({result.returncode})\n{result.stdout}{result.stderr}"
        )
    return result


def git(root, env, *args):
    return run(["git", *args], root, env).stdout.strip()


def setup_repo(parent, env):
    root = parent / "repo"
    root.mkdir(parents=True)
    git(root, env, "init", "-q", "--initial-branch=main")
    git(root, env, "config", "user.name", "Waku Benchmark")
    git(root, env, "config", "user.email", "benchmark@example.invalid")
    git(root, env, "config", "core.fsmonitor", "false")
    git(root, env, "config", "gc.auto", "0")
    (root / "README").write_text("benchmark fixture\n")
    git(root, env, "add", ".")
    git(root, env, "commit", "-qm", "initial")
    return root


def setup_clean(parent, env, count):
    root = setup_repo(parent, env)
    remote = parent / "origin.git"
    git(root, env, "init", "--bare", "-q", "--initial-branch=main", str(remote))
    git(root, env, "remote", "add", "origin", str(remote))
    git(root, env, "push", "-qu", "origin", "main")
    git(root, env, "symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main")
    base = git(root, env, "rev-parse", "HEAD")
    branches = []
    for index in range(count):
        kind = ("unchanged", "squashed", "gone")[index % 3]
        branch = f"{kind}-{index:02}"
        branches.append(branch)
        worktree = parent / "repo-worktrees" / branch
        git(root, env, "worktree", "add", "-q", "-b", branch, str(worktree), base)
        if kind != "unchanged":
            name = f"{branch}.txt"
            (worktree / name).write_text(f"content for {branch}\n")
            git(worktree, env, "add", name)
            git(worktree, env, "commit", "-qm", branch)
            if kind == "squashed":
                (root / name).write_text(f"content for {branch}\n")
            else:
                git(root, env, "config", f"branch.{branch}.remote", "origin")
                git(root, env, "config", f"branch.{branch}.merge", f"refs/heads/{branch}")
    git(root, env, "add", ".")
    git(root, env, "commit", "-qm", "squash changes")
    git(root, env, "push", "-q", "origin", "main")
    return root, branches


def setup_copy(parent, env, count, directory=False):
    root = setup_repo(parent, env)
    (root / ".gitignore").write_text("cache/\n")
    (root / ".worktreeinclude").write_text("cache/**/*.dat\n")
    git(root, env, "add", ".")
    git(root, env, "commit", "-qm", "include ignored cache")
    if directory:
        git(root, env, "config", "waku.worktreeinclude", "ignore")
        git(root, env, "config", "waku.copy.include", "cache")
    for index in range(count):
        path = root / "cache" / f"group-{index % 16:02}" / f"file-{index:04}.dat"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(f"file {index}\n" + "x" * 256)
    return root


def file_manifest(root):
    result = []
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root)
        if relative.parts[0] == ".git":
            continue
        if path.is_file():
            result.append((str(relative), hashlib.sha256(path.read_bytes()).hexdigest()))
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", action="append", required=True, metavar="LABEL=PATH")
    parser.add_argument("--repetitions", type=int, default=5)
    parser.add_argument("--candidates", type=int, default=48)
    parser.add_argument("--files", type=int, default=640)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if args.repetitions < 1 or args.candidates < 3 or args.files < 1:
        parser.error("repetitions/files must be positive; candidates must be at least 3")
    binaries = []
    for spec in args.binary:
        label, separator, path = spec.partition("=")
        binary = Path(path).resolve()
        if not separator or not label or not binary.is_file():
            parser.error(f"expected LABEL=PATH to an existing binary: {spec}")
        if label in [entry[0] for entry in binaries]:
            parser.error(f"duplicate label: {label}")
        binaries.append((label, binary))

    env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    env.update({
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_TERMINAL_PROMPT": "0",
        "GIT_AUTHOR_DATE": "2026-01-01T00:00:00+00:00",
        "GIT_COMMITTER_DATE": "2026-01-01T00:00:00+00:00",
        "LC_ALL": "C", "LANG": "C", "TERM": "dumb", "NO_COLOR": "1",
        "CLICOLOR": "0", "CLICOLOR_FORCE": "0",
    })
    results = {}
    evidence = {}

    def measure(name, root, command, prepare=lambda: None, verify=lambda result: None):
        samples = {label: [] for label, _ in binaries}
        expected = None
        # Alternate binary order between rounds to reduce systematic cache bias.
        for round_index in range(args.repetitions + 1):
            order = binaries if round_index % 2 == 0 else list(reversed(binaries))
            for label, binary in order:
                prepare()
                start = time.perf_counter()
                result = run([binary, *command], root, env)
                elapsed = (time.perf_counter() - start) * 1000
                signature = verify(result)
                if expected is None:
                    expected = signature
                elif signature != expected:
                    raise AssertionError(f"behavior differs in {name} ({label})")
                if round_index:
                    samples[label].append(elapsed)
        results[name] = {
            label: {"median_ms": round(statistics.median(values), 3),
                    "samples_ms": [round(value, 3) for value in values]}
            for label, values in samples.items()
        }
        evidence[name] = "verified on every sample and warmup"
        print(name + ": " + ", ".join(
            f"{label}={data['median_ms']:.3f} ms" for label, data in results[name].items()
        ), flush=True)

    with tempfile.TemporaryDirectory(prefix="waku-benchmark-") as temporary:
        parent = Path(temporary).resolve()
        clean_root, branches = setup_clean(parent / "clean", env, args.candidates)
        initial_worktrees = git(clean_root, env, "worktree", "list", "--porcelain")

        def verify_clean(result):
            for branch in branches:
                if branch not in result.stdout:
                    raise AssertionError(f"missing candidate {branch}: {result.stdout}")
            if git(clean_root, env, "worktree", "list", "--porcelain") != initial_worktrees:
                raise AssertionError("dry-run changed registered worktrees")
            return result.stdout

        measure(f"clean_dry_run_{args.candidates}", clean_root,
                ["clean", "--dry-run"], verify=verify_clean)
        selected = branches[0]
        selected_path = clean_root.parent / "repo-worktrees" / selected

        def verify_path(result):
            if Path(result.stdout.strip()) != selected_path:
                raise AssertionError(f"wrong worktree path: {result.stdout}")
            return result.stdout

        measure(f"path_{args.candidates}", clean_root, ["path", selected], verify=verify_path)

        def verify_open(result):
            if git(clean_root, env, "worktree", "list", "--porcelain") != initial_worktrees:
                raise AssertionError("open changed registered worktrees")
            return result.stdout

        measure(f"open_{args.candidates}", clean_root,
                ["open", selected, "--editor=/usr/bin/true"],
                verify=verify_open)
        for _, binary in binaries:
            verify_path(run([binary, "open", selected, "--editor=/bin/pwd"], clean_root, env))
        evidence[f"open_{args.candidates}"] += "; launched directory checked per binary"

        for scenario, root in (
            ("create", setup_repo(parent / "plain", env)),
            (f"create_include_{args.files}", setup_copy(parent / "copy", env, args.files)),
            (f"create_copy_dir_{args.files}",
             setup_copy(parent / "copy-dir", env, args.files, directory=True)),
        ):
            expected_files = file_manifest(root)
            worktree = root.parent / "repo-worktrees" / "bench"

            def prepare_create():
                if worktree.exists():
                    git(root, env, "worktree", "remove", "--force", str(worktree))
                    git(root, env, "branch", "-D", "bench")

            def verify_create(result):
                actual = file_manifest(worktree)
                if actual != expected_files:
                    raise AssertionError(f"file contents differ after {scenario}")
                branch = git(worktree, env, "branch", "--show-current")
                status = git(worktree, env, "status", "--porcelain")
                if branch != "bench" or status:
                    raise AssertionError(f"unexpected checkout state: {branch}, {status}")
                return actual

            measure(scenario, root, ["create", "bench"], prepare_create, verify_create)
            prepare_create()

            if scenario == "create":
                def prepare_remove():
                    git(root, env, "worktree", "add", "-q", "-b", "bench", str(worktree))

                def verify_remove(result):
                    if worktree.exists():
                        raise AssertionError("remove left the worktree directory")
                    if git(root, env, "branch", "--list", "bench"):
                        raise AssertionError("remove left the branch")
                    registered = git(root, env, "worktree", "list", "--porcelain")
                    if registered.count("worktree ") != 1:
                        raise AssertionError("remove left registered worktree metadata")
                    return result.stdout

                measure("remove", root, ["remove", "bench"], prepare_remove, verify_remove)

    report = {
        "repetitions": args.repetitions,
        "warmups": 1,
        "candidates": args.candidates,
        "files": args.files,
        "binaries": {label: str(binary) for label, binary in binaries},
        "binary_sha256": {
            label: hashlib.sha256(binary.read_bytes()).hexdigest()
            for label, binary in binaries
        },
        "platform": platform.platform(),
        "git_version": run(["git", "--version"], Path.cwd(), env).stdout.strip(),
        "results": results,
        "behavior_checks": evidence,
    }
    if args.output:
        args.output.write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
