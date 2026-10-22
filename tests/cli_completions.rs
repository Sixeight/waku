use std::process::Command;

#[test]
fn completions_support_all_shells_without_internal_commands() {
    for shell in ["bash", "elvish", "fish", "powershell", "zsh"] {
        let output = Command::new(env!("CARGO_BIN_EXE_git-waku"))
            .args(["completions", shell])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{shell}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let completion = String::from_utf8(output.stdout).unwrap();
        assert!(completion.contains("git-waku"), "{shell}");
        assert!(completion.contains("create"), "{shell}");
        assert!(completion.contains("copy-from-base"), "{shell}");
        assert!(!completion.contains("__background"), "{shell}");
    }
}
