#[allow(dead_code)]
mod agents {
    include!("../src/cmd/clean/agents.rs");

    pub(super) fn run(root: &std::path::Path) -> anyhow::Result<()> {
        let directories: Vec<_> = (0..32)
            .map(|index| {
                root.join("agents")
                    .join(index.to_string())
                    .join("nested/workspace")
            })
            .collect();
        let worktrees: Vec<_> = (0..192)
            .map(|index| root.join("worktrees").join(index.to_string()))
            .collect();
        for path in directories.iter().chain(&worktrees) {
            std::fs::create_dir_all(path)?;
        }
        let snapshot = ActiveAgents::from_directories(directories.clone());
        let mut baseline = Vec::new();
        let mut candidate = Vec::new();
        for round in 0..8 {
            for variant in if round % 2 == 0 { [0, 1] } else { [1, 0] } {
                let started = std::time::Instant::now();
                for _ in 0..4 {
                    for worktree in &worktrees {
                        let uses = if variant == 0 {
                            let worktree = normalized(worktree);
                            directories
                                .iter()
                                .any(|cwd| normalized(cwd).starts_with(&worktree))
                        } else {
                            snapshot.uses(worktree)
                        };
                        assert!(!std::hint::black_box(uses));
                    }
                }
                let elapsed = started.elapsed().as_secs_f64() * 1000.0;
                if round != 0 {
                    if variant == 0 {
                        baseline.push(elapsed);
                    } else {
                        candidate.push(elapsed);
                    }
                }
            }
        }
        println!("{{\"agents\":32,\"worktrees\":192,\"passes\":4,\"warmups\":1,\"baseline_ms\":{baseline:?},\"candidate_ms\":{candidate:?}}}");
        Ok(())
    }
}

fn main() -> anyhow::Result<()> {
    let root = std::env::args_os()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("expected fixture directory"))?;
    agents::run(std::path::Path::new(&root))
}
