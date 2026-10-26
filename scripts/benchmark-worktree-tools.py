#!/usr/bin/env python3
"""Compare worktree creation across git-waku and other worktree tools.

Example:
    python3 scripts/benchmark-worktree-tools.py --waku target/release/git-waku \
        --work-dir ~/.cache/waku-tools-benchmark --output /tmp/waku-tools.json

Tools are looked up on PATH; missing ones are skipped. Each sample uses a fresh
HOME and fixture, runs one untimed warmup create, then times one create until
the tool process exits. The timed worktree is verified on every sample.

Scenarios:
    plain    no file sharing
    copy     640 small gitignored files in node_modules/, copied
    link     the same node_modules/, symlinked (tools that support it)
    heavy    ~5,000 files, ~200 MB in node_modules/, copied
    fetch    local origin answering after --latency seconds, with one commit the
             local clone lacks
    real     clone of --real-repo rewound by --real-rewind commits, fetching
             from the real remote

Copy speed depends on the filesystem: on btrfs, XFS, and APFS most tools clone
file data instead of copying it. Run with --work-dir on the filesystem you want
to measure; the system temporary directory may be tmpfs.
"""

import argparse
import json
import os
from pathlib import Path
import platform
import shutil
import statistics
import subprocess
import tempfile
import time

ALL_SCENARIOS = ("plain", "copy", "link", "heavy", "fetch", "real")
SMALL_FILES = 640


def run(args, cwd=None, env=None, check=True):
    result = subprocess.run([str(arg) for arg in args], cwd=cwd, env=env,
                            stdin=subprocess.DEVNULL, capture_output=True, text=True)
    if check and result.returncode:
        raise RuntimeError(f"{args!r} failed ({result.returncode})\n{result.stdout}{result.stderr}")
    return result


def git(root, *args):
    env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    env.update({"GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1",
                "GIT_TERMINAL_PROMPT": "0"})
    return run(["git", "-C", root, *args], env=env).stdout.strip()


def commit(root, message):
    git(root, "-c", "user.name=Waku Benchmark", "-c", "user.email=benchmark@example.invalid",
        "commit", "-qm", message)


def write(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


def count_files(root):
    return sum(len(files) for _, _, files in os.walk(root, followlinks=True))


# Fixtures --------------------------------------------------------------------

def make_small_files(root):
    for group in range(16):
        for index in range(SMALL_FILES // 16):
            write(root / f"pkg{group}" / f"f{index}.js", f"module {group} {index}\n")


def ensure_heavy_source(path):
    """~5,000 files, ~200 MB: many small files plus one 2 MB file per package."""
    if (path / ".complete").exists():
        return
    shutil.rmtree(path, ignore_errors=True)
    for package in range(100):
        lib = path / f"pkg{package}" / "lib" / "sub"
        lib.mkdir(parents=True)
        for index in range(49):
            (lib / f"f{index}.js").write_bytes(os.urandom(1024 + (index * 157) % 7168))
        (path / f"pkg{package}" / "bundle.bin").write_bytes(os.urandom(2 * 1024 * 1024))
    (path / ".complete").write_text("")


def is_ancestor(root, ancestor, descendant):
    return run(["git", "-C", root, "merge-base", "--is-ancestor", ancestor, descendant],
               check=False).returncode == 0


def ensure_real_template(work_dir, url, rewind):
    """Clone url once, then keep a template holding only the objects up to HEAD~rewind,
    so that a fetch from url has to download the newer commits."""
    cache = work_dir / "real-cache"
    if not (cache / ".git").exists():
        print(f"cloning {url} ...", flush=True)
        run(["git", "clone", "-q", url, cache])
    branch = git(cache, "symbolic-ref", "--short", "HEAD")
    tip = git(cache, "rev-parse", "HEAD")
    base = git(cache, "rev-parse", f"HEAD~{rewind}")
    template = work_dir / "real-template"
    shutil.rmtree(template, ignore_errors=True)
    git(cache, "branch", "-f", "waku-benchmark-base", base)
    run(["git", "clone", "-q", "--bare", "--no-local", "--single-branch",
         "--branch", "waku-benchmark-base", cache, template])
    git(template, "branch", "-m", "waku-benchmark-base", branch)
    if run(["git", "-C", template, "cat-file", "-e", tip], check=False).returncode == 0:
        raise RuntimeError("real template unexpectedly contains the remote tip")
    return {"template": template, "branch": branch, "base": base, "remote_tip": tip}


def make_fixture(parent, scenario, args):
    """Return (repo, info). info["remote_tip"] is the commit a fetch would bring."""
    repo = parent / "myrepo"
    if scenario == "real":
        real = args.real
        run(["git", "clone", "-q", "--local", real["template"], repo])
        git(repo, "remote", "set-url", "origin", args.real_repo)
        return repo, {key: real[key] for key in ("branch", "base", "remote_tip")}

    origin = parent / "origin.git"
    run(["git", "init", "-q", "--bare", "-b", "main", origin])
    run(["git", "init", "-q", "-b", "main", repo])
    for index in range(20):
        write(repo / "src" / f"f{index}.txt", f"file {index}\n")
    write(repo / "README.md", "# fixture\n")
    write(repo / ".gitignore", "node_modules/\n")
    if scenario in ("copy", "link", "heavy"):
        # workz only shares node_modules/ in a Node project.
        write(repo / "package.json", '{"name": "fixture"}\n')
    git(repo, "add", ".")
    commit(repo, "initial")
    git(repo, "remote", "add", "origin", origin)
    git(repo, "push", "-q", "origin", "main")
    git(repo, "fetch", "-q", "origin")
    git(repo, "branch", "-q", "--set-upstream-to=origin/main", "main")
    base = git(repo, "rev-parse", "HEAD")
    info = {"remote_tip": base, "base": base, "branch": "main"}
    if scenario in ("copy", "link"):
        make_small_files(repo / "node_modules")
    elif scenario == "heavy":
        shutil.copytree(args.heavy_source, repo / "node_modules",
                        ignore=shutil.ignore_patterns(".complete"))
    elif scenario == "fetch":
        upstream = parent / "upstream"
        run(["git", "clone", "-q", origin, upstream])
        write(upstream / "NEW.md", "remote only\n")
        git(upstream, "add", ".")
        commit(upstream, "remote only")
        git(upstream, "push", "-q", "origin", "main")
        info["remote_tip"] = git(upstream, "rev-parse", "HEAD")
        git(repo, "config", "protocol.ext.allow", "always")
        # ext:: splits arguments on spaces ("% " is a literal space, "%%" a literal %),
        # and sh splits the script again, so the path is also single-quoted.
        escaped = str(origin).replace("%", "%%").replace(" ", "% ")
        git(repo, "remote", "set-url", "origin",
            f"ext::sh -c sleep% {args.latency};% exec% git% %s% '{escaped}'")
    return repo, info


# Tools -----------------------------------------------------------------------
# Each tool lists the scenarios it supports; setup(repo, home, scenario)
# configures it and command(branch, scenario) returns argv.

FETCH_SCENARIOS = ("fetch", "real")
COPY_SCENARIOS = ("copy", "heavy")


def setup_waku(repo, home, scenario):
    if scenario in COPY_SCENARIOS:
        write(repo / ".worktreeinclude", "node_modules/\n")
    elif scenario == "link":
        git(repo, "config", "waku.link.include", "node_modules")
    elif scenario in FETCH_SCENARIOS:
        git(repo, "config", "waku.create.fetch", "true")
        git(repo, "config", "waku.create.from-default-branch", "true")


def setup_waku_sync_fetch(repo, home, scenario):
    git(repo, "config", "waku.create.from-default-branch", "true")


def setup_worktrunk(repo, home, scenario):
    # pre-start blocks until the copy finishes. worktrunk documents post-start,
    # which returns earlier while the copy continues in the background.
    if scenario in COPY_SCENARIOS:
        write(home / ".config/worktrunk/config.toml", 'pre-start = "wt step copy-ignored"\n')


def setup_gtr(repo, home, scenario):
    if scenario in COPY_SCENARIOS:
        git(repo, "config", "gtr.copy.includeDirs", "node_modules")


def setup_git_wt(repo, home, scenario):
    # wt.symlink only applies to paths that wt.copy already selects.
    git(repo, "config", "wt.copy", "node_modules/")
    if scenario == "link":
        git(repo, "config", "wt.symlink", "node_modules/")


def setup_gwq(repo, home, scenario):
    # gwq builds worktree paths from the origin URL, which must look like host/owner/repo.
    if scenario != "real":
        git(repo, "remote", "set-url", "origin", "git@example.invalid:bench/myrepo.git")
    config = f'[worktree]\nbasedir = "{repo.parent}/gwq-worktrees"\nauto_mkdir = true\n'
    if scenario in COPY_SCENARIOS:
        config += (f'[[repository_settings]]\nrepository = "{repo}"\n'
                   'copy_files = ["node_modules/**"]\n')
    write(home / ".config/gwq/config.toml", config)


def setup_wtp(repo, home, scenario):
    config = 'version: "1.0"\ndefaults:\n  base_dir: "../myrepo-worktrees"\n'
    if scenario in COPY_SCENARIOS:
        config += 'hooks:\n  post_create:\n    - type: copy\n      from: "node_modules"\n'
    elif scenario == "link":
        config += ('hooks:\n  post_create:\n    - type: symlink\n'
                   '      from: "node_modules"\n      to: "node_modules"\n')
    write(repo / ".wtp.yml", config)


def setup_rabi(repo, home, scenario):
    if scenario in COPY_SCENARIOS:
        write(repo / ".worktreeinclude", "node_modules/\n")


def setup_workz(repo, home, scenario):
    # workz symlinks node_modules/ by default.
    if scenario in COPY_SCENARIOS:
        write(repo / ".workz.toml", '[sync.overrides]\nnode_modules = "copy"\n')


WITHOUT_LINK = tuple(s for s in ALL_SCENARIOS if s != "link")
TOOLS = {
    "waku": ("git-waku", ALL_SCENARIOS, setup_waku,
             lambda b, s: ["git", "waku", "create", b]),
    "waku --fetch": ("git-waku", FETCH_SCENARIOS, setup_waku_sync_fetch,
                     lambda b, s: ["git", "waku", "create", b, "--fetch"]),
    "worktrunk": ("wt", WITHOUT_LINK, setup_worktrunk,
                  lambda b, s: ["wt", "switch", "--create", b, "--no-cd"]),
    "gtr": ("git-gtr", WITHOUT_LINK, setup_gtr,
            lambda b, s: ["git", "gtr", "new", b, "--yes"]
            + ([] if s in FETCH_SCENARIOS else ["--no-fetch"])),
    "git-wt": ("git-wt", ALL_SCENARIOS, setup_git_wt, lambda b, s: ["git", "wt", "--nocd", b]),
    "gwq": ("gwq", ("plain", "copy", "heavy", "real"), setup_gwq,
            lambda b, s: ["gwq", "add", "-b", b]),
    "wtp": ("wtp", ALL_SCENARIOS, setup_wtp, lambda b, s: ["wtp", "add", "-b", b]),
    "rabi": ("rabi", WITHOUT_LINK, setup_rabi,
             lambda b, s: ["rabi", "create", b, "--no-input"]),
    "workz": ("workz", ALL_SCENARIOS, setup_workz, lambda b, s: ["workz", "start", b]),
}

VERSION_COMMANDS = {
    "git-waku": ["git-waku", "--version"], "wt": ["wt", "--version"],
    "git-gtr": ["git-gtr", "version"], "git-wt": ["git-wt", "--version"],
    "gwq": ["gwq", "version"], "wtp": ["wtp", "--version"],
    "rabi": ["rabi", "--version"], "workz": ["workz", "--version"],
}


def tool_env(home, path):
    return {
        "PATH": path, "HOME": str(home), "TERM": "dumb", "NO_COLOR": "1", "LC_ALL": "C",
        "XDG_CONFIG_HOME": f"{home}/.config", "XDG_STATE_HOME": f"{home}/.local/state",
        "XDG_DATA_HOME": f"{home}/.local/share", "XDG_CACHE_HOME": f"{home}/.cache",
        "GIT_CONFIG_GLOBAL": f"{home}/.gitconfig", "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_TERMINAL_PROMPT": "0",
    }


# Verification ----------------------------------------------------------------

def worktree_path(repo, branch):
    path = None
    for line in git(repo, "worktree", "list", "--porcelain").splitlines():
        if line.startswith("worktree "):
            path = line[len("worktree "):]
        elif line == f"branch refs/heads/{branch}":
            return Path(path)
    return None


def verify(repo, branch, scenario, info, expected_files):
    """Raise unless the worktree is usable; return whether it has the remote tip."""
    worktree = worktree_path(repo, branch)
    if worktree is None or not worktree.is_dir():
        raise AssertionError("worktree not registered")
    head = git(worktree, "rev-parse", "HEAD")
    if not is_ancestor(worktree, info["base"], head):
        raise AssertionError(f"unexpected base {head[:12]}")
    if git(worktree, "status", "--porcelain", "--untracked-files=no"):
        raise AssertionError("checkout incomplete")
    if expected_files:
        node_modules = worktree / "node_modules"
        kind = ("symlink" if node_modules.is_symlink()
                else "dir" if node_modules.is_dir() else "missing")
        files = count_files(node_modules) if kind != "missing" else 0
        wanted = "symlink" if scenario == "link" else "dir"
        if kind != wanted or files != expected_files:
            raise AssertionError(f"node_modules is {kind} with {files} files")
    # The real remote may have moved past the recorded tip, so check ancestry.
    return (head != info["base"]
            and run(["git", "-C", worktree, "cat-file", "-e", info["remote_tip"]],
                    check=False).returncode == 0
            and is_ancestor(worktree, info["remote_tip"], head))


# Measurement -----------------------------------------------------------------

def one_sample(tool, scenario, index, args, path, expected_files):
    _, _, setup, command = TOOLS[tool]
    parent = args.work_dir / f"{scenario}-{tool.replace(' ', '_')}-{index}"
    shutil.rmtree(parent, ignore_errors=True)
    home = parent / "home"
    write(home / ".gitconfig", "[user]\n\tname = Waku Benchmark\n\temail = benchmark@example.invalid\n")
    try:
        repo, info = make_fixture(parent / "fixture", scenario, args)
        setup(repo, home, scenario)
        env = tool_env(home, path)
        settle = args.real_settle if scenario == "real" else 1.0
        remote_ref = f"refs/remotes/origin/{info['branch']}"

        def create(branch):
            # Output goes to a file: a background child that inherits a pipe
            # would keep it open after the tool exits and inflate the timing.
            log = parent / f"{branch}.log"
            with open(log, "w") as output:
                process = subprocess.Popen(command(branch, scenario), cwd=repo, env=env,
                                           stdin=subprocess.DEVNULL, stdout=output,
                                           stderr=subprocess.STDOUT)
                start = time.perf_counter()
                code = process.wait()
                elapsed = (time.perf_counter() - start) * 1000
            if code:
                raise AssertionError(f"{branch} exited {code}: {log.read_text()[-400:]}")
            return elapsed

        create("warmup")
        if scenario in FETCH_SCENARIOS:
            time.sleep(settle)
            # Undo whatever the warmup fetched so the timed create starts stale.
            git(repo, "update-ref", remote_ref, info["base"])
            time.sleep(0.2)
            if git(repo, "rev-parse", remote_ref) != info["base"]:
                raise AssertionError("warmup fetch still running; raise the settle time")
        elapsed = create("bench")
        # The worktree must be usable when the tool exits, not only after background work.
        sample = {"ms": elapsed,
                  "fresh_at_exit": verify(repo, "bench", scenario, info, expected_files)}
        if scenario in FETCH_SCENARIOS:
            time.sleep(settle)
        sample["fresh_after_settle"] = verify(repo, "bench", scenario, info, expected_files)
        return sample
    except (AssertionError, RuntimeError) as error:
        return {"error": str(error)}
    finally:
        shutil.rmtree(parent, ignore_errors=True)


def summarize(scenario, tool, samples):
    passed = [sample for sample in samples if "error" not in sample]
    summary = {"samples": len(samples), "failed": len(samples) - len(passed),
               "errors": sorted({sample["error"][:200] for sample in samples
                                 if "error" in sample})}
    line = f"{scenario:6} {tool:13}"
    if passed:
        values = [sample["ms"] for sample in passed]
        summary.update(median_ms=round(statistics.median(values), 1),
                       min_ms=round(min(values), 1), max_ms=round(max(values), 1),
                       samples_ms=[round(value, 1) for value in values])
        line += (f" {summary['median_ms']:8.1f} ms "
                 f"({summary['min_ms']:.1f}-{summary['max_ms']:.1f})")
        if scenario in FETCH_SCENARIOS:
            for key in ("fresh_at_exit", "fresh_after_settle"):
                summary[key] = sum(sample[key] for sample in passed)
            line += (f"  fresh at exit {summary['fresh_at_exit']}/{len(passed)}, "
                     f"after settle {summary['fresh_after_settle']}/{len(passed)}")
    if summary["failed"]:
        line += f"  FAILED {summary['failed']}/{len(samples)}: {summary['errors'][0]}"
    print(line, flush=True)
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--waku", type=Path, default=Path("target/release/git-waku"))
    parser.add_argument("--tools", default=",".join(TOOLS))
    parser.add_argument("--scenarios", default=",".join(ALL_SCENARIOS))
    parser.add_argument("--repetitions", type=int, default=15)
    parser.add_argument("--heavy-repetitions", type=int, default=7)
    parser.add_argument("--real-repetitions", type=int, default=10)
    parser.add_argument("--work-dir", type=Path)
    parser.add_argument("--latency", default="0.3", help="seconds of origin latency in fetch")
    parser.add_argument("--real-repo", default="https://github.com/django/django.git")
    parser.add_argument("--real-rewind", type=int, default=20)
    parser.add_argument("--real-settle", type=float, default=5.0,
                        help="seconds to wait for background fetches in real")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()

    waku = args.waku.resolve()
    if not waku.is_file():
        parser.error(f"git-waku binary not found: {args.waku}")
    scenarios = args.scenarios.split(",")
    if set(scenarios) - set(ALL_SCENARIOS):
        parser.error(f"unknown scenario in {args.scenarios}")
    if min(args.repetitions, args.heavy_repetitions, args.real_repetitions, args.real_rewind) < 1:
        parser.error("repetitions and --real-rewind must be positive")
    for tool in args.tools.split(","):
        if tool not in TOOLS:
            parser.error(f"unknown tool: {tool}")

    temporary = None
    if args.work_dir is None:
        temporary = tempfile.TemporaryDirectory(prefix="waku-tools-")
        args.work_dir = Path(temporary.name)
    try:
        report = benchmark(args, waku, scenarios, parser)
    finally:
        if temporary:
            temporary.cleanup()
    if args.output:
        args.output.write_text(json.dumps(report, indent=2) + "\n")


def benchmark(args, waku, scenarios, parser):
    args.work_dir = args.work_dir.expanduser().resolve()
    args.work_dir.mkdir(parents=True, exist_ok=True)
    bin_dir = args.work_dir / "bin"
    bin_dir.mkdir(exist_ok=True)
    (bin_dir / "git-waku").unlink(missing_ok=True)
    (bin_dir / "git-waku").symlink_to(waku)
    path = f"{bin_dir}{os.pathsep}{os.environ['PATH']}"

    tools = []
    for tool in args.tools.split(","):
        if shutil.which(TOOLS[tool][0], path=path):
            tools.append(tool)
        else:
            print(f"skipping {tool}: {TOOLS[tool][0]} not on PATH", flush=True)
    if not tools:
        parser.error("none of the selected tools is on PATH")
    versions = {}
    for executable, command in VERSION_COMMANDS.items():
        if shutil.which(executable, path=path):
            result = run(command, env={"PATH": path, "HOME": str(args.work_dir)}, check=False)
            lines = (result.stdout or result.stderr).strip().splitlines()
            versions[executable] = lines[0] if lines else "unknown"

    results = {}
    for scenario in scenarios:
        repetitions = args.repetitions
        expected_files = SMALL_FILES if scenario in ("copy", "link") else 0
        if scenario == "heavy":
            args.heavy_source = args.work_dir / "heavy-source"
            ensure_heavy_source(args.heavy_source)
            expected_files = count_files(args.heavy_source) - 1
            repetitions = args.heavy_repetitions
        elif scenario == "real":
            args.real = ensure_real_template(args.work_dir, args.real_repo, args.real_rewind)
            repetitions = args.real_repetitions
        runnable = [tool for tool in tools if scenario in TOOLS[tool][1]]
        if not runnable:
            print(f"skipping {scenario}: no selected tool supports it", flush=True)
            continue
        samples = {tool: [] for tool in runnable}
        for round_index in range(repetitions):
            offset = round_index % len(runnable)
            for tool in runnable[offset:] + runnable[:offset]:
                samples[tool].append(
                    one_sample(tool, scenario, round_index, args, path, expected_files))
        results[scenario] = {tool: summarize(scenario, tool, samples[tool]) for tool in runnable}

    return {
        "platform": platform.platform(),
        "git_version": run(["git", "--version"]).stdout.strip(),
        "work_dir": str(args.work_dir),
        "versions": versions,
        "repetitions": {"default": args.repetitions, "heavy": args.heavy_repetitions,
                        "real": args.real_repetitions},
        "latency_s": args.latency,
        "real_repo": args.real_repo,
        "real_rewind": args.real_rewind,
        "results": results,
    }


if __name__ == "__main__":
    main()
