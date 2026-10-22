use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

pub(super) struct ActiveAgents {
    directories: Vec<PathBuf>,
}

impl ActiveAgents {
    pub(super) fn inspect() -> Result<Self> {
        let output = Command::new("ps")
            .args(["-ww", "-axo", "pid=,args="])
            .output()
            .context("failed to list processes")?;
        anyhow::ensure!(output.status.success(), "ps failed ({})", output.status);
        let executables = Command::new("ps")
            .args(["-ww", "-axo", "pid=,comm="])
            .output()
            .context("failed to list process executables")?;
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

fn executable_pid(line: &str) -> Option<u32> {
    let (pid, executable) = process(line)?;
    agent_path(Path::new(executable)).then_some(pid)
}

fn script_agent(args: &str) -> bool {
    let mut words = args.split_whitespace();
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
