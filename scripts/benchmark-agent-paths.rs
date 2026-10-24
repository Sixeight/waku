#[allow(dead_code)]
mod agents {
    include!("../src/cmd/clean/agents.rs");

    const AGENTS: usize = 32;
    const WORKTREES: usize = 192;
    const PASSES: usize = 4;
    const WARMUPS: usize = 1;
    const SAMPLES: usize = 7;
    const ROUNDS: usize = SAMPLES + WARMUPS;

    pub(super) fn run(root: &std::path::Path) -> anyhow::Result<()> {
        let directories: Vec<_> = (0..AGENTS)
            .map(|index| {
                root.join("agents")
                    .join(index.to_string())
                    .join("nested/workspace")
            })
            .collect();
        let worktrees: Vec<_> = (0..WORKTREES)
            .map(|index| root.join("worktrees").join(index.to_string()))
            .collect();
        for path in directories.iter().chain(&worktrees) {
            std::fs::create_dir_all(path)?;
        }
        let snapshot = ActiveAgents::from_directories(directories.clone());
        let mut samples = [Vec::new(), Vec::new()];
        for round in 0..ROUNDS {
            for variant in if round % 2 == 0 { [0, 1] } else { [1, 0] } {
                let started = std::time::Instant::now();
                for _ in 0..PASSES {
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
                if round >= WARMUPS {
                    samples[variant].push(elapsed);
                }
            }
        }
        let [baseline, candidate] = samples;
        println!("{{\"agents\":{AGENTS},\"worktrees\":{WORKTREES},\"passes\":{PASSES},\"warmups\":{WARMUPS},\"baseline_ms\":{baseline:?},\"candidate_ms\":{candidate:?}}}");
        Ok(())
    }
}

fn main() -> anyhow::Result<()> {
    let root = std::env::args_os()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("expected fixture directory"))?;
    agents::run(std::path::Path::new(&root))
}
