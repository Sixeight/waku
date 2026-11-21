use std::path::{Path, PathBuf};
#[cfg(any(not(target_os = "linux"), test))]
use std::process::Command;

use anyhow::{Context, Result};

pub(super) struct ActiveAgents {
    directories: Vec<PathBuf>,
}

impl ActiveAgents {
    pub(super) fn inspect() -> Result<Self> {
        let pids = agent_pids()?;
        Ok(Self::from_directories(process_directories(&pids)?))
    }

    fn from_directories(directories: Vec<PathBuf>) -> Self {
        let mut directories: Vec<_> = directories.iter().map(|path| normalized(path)).collect();
        directories.sort_unstable();
        directories.dedup();
        Self { directories }
    }

    pub(super) fn uses(&self, worktree: &Path) -> bool {
        let worktree = normalized(worktree);
        self.directories
            .iter()
            .any(|cwd| cwd.starts_with(&worktree))
    }

    pub(super) fn ensure_unused(&self, worktree: &Path) -> Result<()> {
        anyhow::ensure!(
            !self.uses(worktree),
            "refusing to delete {}: active AI agent session",
            worktree.display()
        );
        Ok(())
    }
}

fn normalized(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(any(not(target_os = "linux"), test))]
fn process(line: &str) -> Option<(u32, &str)> {
    let line = line.trim();
    let (pid, command) = line.split_once(char::is_whitespace)?;
    Some((pid.parse().ok()?, command.trim()))
}

fn agent_name(name: &str) -> bool {
    matches!(
        name,
        "claude" | "codex" | "opencode" | "gemini" | "aider" | "grok"
    )
}

#[cfg(any(not(target_os = "linux"), test))]
fn agent_pid(line: &str) -> Option<u32> {
    let (pid, command) = process(line)?;
    let executable = command.split_whitespace().next()?;
    (agent_path(Path::new(executable)) || script_agent(command)).then_some(pid)
}

fn agent_path(path: &Path) -> bool {
    let name_matches = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(agent_name);
    let versioned_claude = path.parent().is_some_and(|parent| {
        parent.file_name().is_some_and(|name| name == "versions")
            && parent
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name == "claude")
    });
    name_matches || versioned_claude
}

#[cfg(any(not(target_os = "linux"), test))]
fn executable_pid(line: &str) -> Option<u32> {
    let (pid, executable) = process(line)?;
    agent_path(Path::new(executable)).then_some(pid)
}

#[cfg(any(not(target_os = "linux"), test))]
fn script_agent(args: &str) -> bool {
    script_agent_words(args.split_whitespace())
}

fn script_agent_words<'a>(mut words: impl Iterator<Item = &'a str>) -> bool {
    let Some(runtime) = words
        .next()
        .and_then(|s| Path::new(s).file_name())
        .and_then(|s| s.to_str())
    else {
        return false;
    };
    if !matches!(runtime, "node" | "python" | "python3") {
        return false;
    }
    let Some(script) = words.next() else {
        return false;
    };
    let name = Path::new(script)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    agent_name(name)
        || matches!(name, "codex.js" | "aider-chat")
        || script.contains("/@anthropic-ai/claude-code/")
        || script.contains("/@google/gemini-cli/")
        || (script == "-m" && words.next() == Some("aider"))
}

#[cfg(target_os = "linux")]
fn agent_pids() -> Result<Vec<u32>> {
    agent_pids_in_proc(Path::new("/proc"))
}

#[cfg(target_os = "linux")]
fn agent_pids_in_proc(proc_root: &Path) -> Result<Vec<u32>> {
    use std::io::Read;

    let mut pids = Vec::new();
    let mut cmdline_buf = [0u8; 4096];
    let mut comm_buf = [0u8; 128];

    let entries = std::fs::read_dir(proc_root)
        .with_context(|| format!("failed to list processes in {}", proc_root.display()))?;
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let mut path = entry.path();
        path.push("cmdline");
        let matched_cmdline = std::fs::File::open(&path)
            .and_then(|mut file| file.read(&mut cmdline_buf))
            .is_ok_and(|n| cmdline_matches_agent(&cmdline_buf[..n]));
        if matched_cmdline {
            pids.push(pid);
            continue;
        }
        path.set_file_name("comm");
        let matched_comm = std::fs::File::open(&path)
            .and_then(|mut file| file.read(&mut comm_buf))
            .is_ok_and(|n| {
                let comm = String::from_utf8_lossy(&comm_buf[..n]);
                agent_path(Path::new(comm.trim()))
            });
        if matched_comm {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    pids.dedup();
    Ok(pids)
}

#[cfg(target_os = "linux")]
fn cmdline_matches_agent(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    let mut args = bytes
        .split(|&b| b == 0)
        .take_while(|part| !part.is_empty())
        .take(3)
        .map(|part| String::from_utf8_lossy(part));
    let Some(first) = args.next() else {
        return false;
    };
    if agent_path(Path::new(first.as_ref())) {
        return true;
    }
    let second = args.next();
    let third = args.next();
    let words = std::iter::once(first.as_ref())
        .chain(second.as_deref())
        .chain(third.as_deref());
    script_agent_words(words)
}

#[cfg(not(target_os = "linux"))]
fn agent_pids() -> Result<Vec<u32>> {
    let (output, executables) = std::thread::scope(|scope| {
        let args = scope.spawn(|| {
            Command::new("ps")
                .args(["-ww", "-axo", "pid=,args="])
                .output()
                .context("failed to list processes")
        });
        let comm = Command::new("ps")
            .args(["-ww", "-axo", "pid=,comm="])
            .output()
            .context("failed to list process executables");
        (args.join().unwrap(), comm)
    });
    let output = output?;
    anyhow::ensure!(output.status.success(), "ps failed ({})", output.status);
    let executables = executables?;
    anyhow::ensure!(
        executables.status.success(),
        "ps executables failed ({})",
        executables.status
    );
    let mut pids: Vec<u32> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(agent_pid)
        .collect();
    pids.extend(
        String::from_utf8_lossy(&executables.stdout)
            .lines()
            .filter_map(executable_pid),
    );
    pids.sort_unstable();
    pids.dedup();
    Ok(pids)
}

#[cfg(target_os = "linux")]
fn process_directories(pids: &[u32]) -> Result<Vec<PathBuf>> {
    let mut directories = Vec::new();
    for pid in pids {
        match std::fs::read_link(format!("/proc/{pid}/cwd")) {
            Ok(path) => directories.push(path),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("failed to read AI agent cwd for PID {pid}"));
            }
        }
    }
    Ok(directories)
}

#[cfg(not(target_os = "linux"))]
fn process_directories(pids: &[u32]) -> Result<Vec<PathBuf>> {
    if pids.is_empty() {
        return Ok(Vec::new());
    }
    let pid_list = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let output = Command::new("lsof")
        .args(["-a", "-p", &pid_list, "-d", "cwd", "-Fn"])
        .output()
        .context("failed to read agent working directories with lsof")?;
    let directories = lsof_directories(&String::from_utf8_lossy(&output.stdout));
    let missing: Vec<_> = pids
        .iter()
        .filter(|pid| !directories.contains_key(pid))
        .collect();
    if !missing.is_empty() {
        let missing_list = missing
            .iter()
            .map(|pid| pid.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let live = Command::new("ps")
            .args(["-p", &missing_list, "-o", "pid=,stat="])
            .output()
            .context("failed to check whether AI agents exited")?;
        anyhow::ensure!(
            live.status.success() || (live.status.code() == Some(1) && live.stderr.is_empty()),
            "failed to inspect remaining AI agents"
        );
        let running = String::from_utf8_lossy(&live.stdout).lines().any(|line| {
            process(line)
                .map(|(_, state)| !state.starts_with('Z'))
                .unwrap_or(!line.trim().is_empty())
        });
        anyhow::ensure!(
            !running,
            "lsof omitted a running AI agent's working directory ({})",
            output.status
        );
    }
    Ok(directories.into_values().collect())
}

#[cfg(any(not(target_os = "linux"), test))]
fn lsof_directories(output: &str) -> std::collections::HashMap<u32, PathBuf> {
    let mut directories = std::collections::HashMap::new();
    let mut pid = None;
    for line in output.lines() {
        if let Some(value) = line.strip_prefix('p') {
            pid = value.parse().ok();
        } else if let (Some(pid), Some(path)) = (pid, line.strip_prefix("n/")) {
            directories.insert(pid, PathBuf::from(format!("/{path}")));
        }
    }
    directories
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_agents_prevent_deletion() {
        let agents = ActiveAgents {
            directories: vec![PathBuf::from("/repo/topic/src")],
        };
        assert!(agents.ensure_unused(Path::new("/repo/topic")).is_err());
        assert!(agents.ensure_unused(Path::new("/repo/topic-other")).is_ok());
    }

    #[test]
    #[cfg(unix)]
    fn agent_snapshot_preserves_canonical_working_directories() {
        let temporary = tempfile::TempDir::new().unwrap();
        let active = temporary.path().join("active");
        let other = temporary.path().join("other");
        let link = temporary.path().join("link");
        std::fs::create_dir(&active).unwrap();
        std::fs::create_dir(&other).unwrap();
        std::os::unix::fs::symlink(&active, &link).unwrap();
        let agents = ActiveAgents::from_directories(vec![link.clone(), active.clone()]);
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&other, &link).unwrap();
        assert!(agents.uses(&active));
        assert!(!agents.uses(&other));
    }

    #[test]
    fn process_detection_matches_executables_and_script_entrypoints() {
        for line in [
            "123 /usr/local/bin/codex --resume",
            "123 claude",
            "123 /home/me/.local/share/claude/versions/2.1.302 --resume",
            "123 node /opt/node_modules/@anthropic-ai/claude-code/cli.js",
            "123 python3 -m aider",
            "123 gemini",
        ] {
            assert_eq!(agent_pid(line), Some(123), "{line}");
        }
        for line in [
            "123 vim /tmp/codex",
            "123 node /tmp/service.js codex",
            "123 /Applications/Codex.app/Contents/MacOS/Codex",
            "bad claude",
        ] {
            assert_eq!(agent_pid(line), None, "{line}");
        }
    }

    #[test]
    fn executable_paths_with_spaces_are_detected_without_matching_arguments() {
        assert_eq!(
            executable_pid(
                "123 /Users/me/Library/Application Support/Claude/claude.app/Contents/MacOS/claude"
            ),
            Some(123)
        );
        assert_eq!(executable_pid("123 /usr/bin/vim"), None);
        assert_eq!(agent_pid("123 vim /tmp/codex"), None);
    }

    #[test]
    fn lsof_output_preserves_spaces_in_paths() {
        assert_eq!(
            lsof_directories("p123\nn/repo/a b\np124\nn/repo/other\n"),
            std::collections::HashMap::from([
                (123, PathBuf::from("/repo/a b")),
                (124, PathBuf::from("/repo/other"))
            ])
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn proc_scan_detects_agents_with_spaces_scripts_and_comm_fallback() {
        let tmp = tempfile::TempDir::new().unwrap();
        let proc_root = tmp.path();
        let write_proc = |pid: &str, cmdline: &[u8], comm: &str| {
            let dir = proc_root.join(pid);
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("cmdline"), cmdline).unwrap();
            std::fs::write(dir.join("comm"), comm).unwrap();
        };
        write_proc("10", b"/opt/Agent Tools/codex\0--resume\0", "codex\n");
        write_proc(
            "11",
            b"node\0/opt/node_modules/@anthropic-ai/claude-code/cli.js\0",
            "node\n",
        );
        write_proc("12", b"python3\0-m\0aider\0", "python3\n");
        write_proc(
            "13",
            b"/home/me/.local/share/claude/versions/2.1.302\0--resume\0",
            "2.1.302\n",
        );
        write_proc("14", b"/usr/bin/wrapper\0", "claude\n");
        write_proc("15", b"vim\0/tmp/codex\0", "vim\n");
        write_proc("16", b"node\0/tmp/service.js\0codex\0", "node\n");
        write_proc("17", b"", "kworker\n");
        std::fs::create_dir(proc_root.join("self")).unwrap();

        assert_eq!(
            agent_pids_in_proc(proc_root).unwrap(),
            vec![10, 11, 12, 13, 14]
        );
        assert!(agent_pids_in_proc(&proc_root.join("missing")).is_err());
    }

    #[test]
    #[cfg(unix)]
    #[ignore = "requires permission to inspect real processes"]
    fn exited_processes_do_not_make_inspection_unavailable() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(process_directories(&[pid]).unwrap().is_empty());
    }
}
