use std::{path::Path, process::Command};

use serde_json::{Value, json};

fn hosts() -> Vec<(&'static str, &'static str, bool)> {
    vec![
        ("gemini-cli", ".gemini/settings.json", false),
        ("qwen", ".qwen/settings.json", false),
        ("droid", ".factory/mcp.json", true),
        ("qoder", ".qoder/settings.json", true),
        ("kiro", ".kiro/settings/mcp.json", false),
    ]
}

fn run(home: &Path, args: &[&str]) -> std::process::Output {
    run_with_env(home, args, &[])
}

fn run_with_env(home: &Path, args: &[&str], variables: &[(&str, &str)]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_confer"))
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("APPDATA", home.join("AppData/Roaming"))
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env("PATH", home.join("bin"))
        .envs(variables.iter().copied())
        .current_dir(home)
        .output()
        .unwrap()
}

fn success(output: std::process::Output) -> String {
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn read(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn explicit_hosts_update_only_confer_and_preserve_configuration() {
    for (host, relative, typed) in hosts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = json!({
            "preferences": {"theme": "dark"},
            "mcpServers": {
                "other": {"url": "https://example.com/mcp"},
                "confer": {"env": {"PRESERVED": "yes"}, "timeout": 12345}
            }
        });
        std::fs::write(&path, original.to_string()).unwrap();
        let before = std::fs::read(&path).unwrap();
        success(run(
            dir.path(),
            &["mcp", "install", "--agent", host, "--dry-run"],
        ));
        success(run(
            dir.path(),
            &["mcp", "uninstall", "--agent", host, "--dry-run"],
        ));
        assert_eq!(std::fs::read(&path).unwrap(), before, "{host}");
        for bin in ["confer", "replacement-confer"] {
            success(run(
                dir.path(),
                &["mcp", "install", "--agent", host, "--bin", bin],
            ));
            let installed = read(&path);
            assert_eq!(installed["preferences"], original["preferences"], "{host}");
            assert_eq!(
                installed["mcpServers"]["other"], original["mcpServers"]["other"],
                "{host}"
            );
            let entry = &installed["mcpServers"]["confer"];
            assert_eq!(entry["command"], bin, "{host}");
            assert_eq!(entry["args"], json!(["mcp"]), "{host}");
            assert_eq!(
                entry["env"], original["mcpServers"]["confer"]["env"],
                "{host}"
            );
            assert_eq!(entry["timeout"], 12345, "{host}");
            assert_eq!(
                entry.get("type"),
                typed.then_some(&json!("stdio")),
                "{host}"
            );
        }
        success(run(dir.path(), &["mcp", "uninstall", "--agent", host]));
        let remaining = read(&path);
        assert!(remaining["mcpServers"].get("confer").is_none(), "{host}");
        assert_eq!(
            remaining["mcpServers"]["other"], original["mcpServers"]["other"],
            "{host}"
        );
        assert_eq!(remaining["preferences"], original["preferences"], "{host}");
        success(run(dir.path(), &["mcp", "uninstall", "--agent", host]));
    }
}

#[test]
fn malformed_and_remote_registrations_are_not_overwritten() {
    for (host, relative, _) in hosts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        for body in [
            "invalid",
            r#"{"mcpServers":[]}"#,
            r#"{"mcpServers":{"confer":false}}"#,
            r#"{"mcpServers":{"confer":{"url":"https://example.com"}}}"#,
            r#"{"mcpServers":{"confer":{"httpUrl":"https://example.com"}}}"#,
            r#"{"mcpServers":{"confer":{"qoder_url":"https://example.com"}}}"#,
        ] {
            std::fs::write(&path, body).unwrap();
            assert!(
                !run(dir.path(), &["mcp", "install", "--agent", host])
                    .status
                    .success(),
                "{host}: {body}"
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), body, "{host}");
        }
    }
}

#[test]
fn discovery_does_not_create_configuration_without_executables() {
    let dir = tempfile::tempdir().unwrap();
    for selection in [
        vec!["mcp", "install"],
        vec!["mcp", "install", "--agent", "*"],
    ] {
        assert!(!run(dir.path(), &selection).status.success());
        for (_, path, _) in hosts() {
            assert!(!dir.path().join(path).exists());
        }
    }
}

#[test]
fn host_configuration_directory_overrides_are_respected() {
    for (host, variable, value, relative) in [
        (
            "gemini-cli",
            "GEMINI_CLI_HOME",
            "custom",
            "custom/.gemini/settings.json",
        ),
        ("qwen", "QWEN_HOME", "custom", "custom/settings.json"),
        ("qwen", "QWEN_HOME", "~/custom", "custom/settings.json"),
        (
            "qoder",
            "QODER_CONFIG_DIR",
            "custom",
            "custom/settings.json",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_confer"))
            .args(["mcp", "install", "--agent", host])
            .env_clear()
            .env("HOME", dir.path())
            .env("USERPROFILE", dir.path())
            .env("PATH", dir.path().join("bin"))
            .env(variable, value)
            .current_dir(dir.path())
            .output()
            .unwrap();
        success(output);
        assert_eq!(
            read(&dir.path().join(relative))["mcpServers"]["confer"]["command"],
            "confer"
        );
    }
}

#[cfg(unix)]
#[test]
fn gemini_discovery_and_legacy_alias_select_distinct_products() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    for name in ["gemini", "agy"] {
        let executable = bin.join(name);
        std::fs::write(&executable, "").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let legacy = success(run(
        dir.path(),
        &["mcp", "install", "--agent", "gemini", "--dry-run"],
    ));
    assert!(legacy.contains("agy mcp add confer confer mcp"), "{legacy}");
    assert!(!dir.path().join(".gemini/settings.json").exists());
    std::fs::remove_file(bin.join("agy")).unwrap();
    success(run(dir.path(), &["mcp", "install"]));
    assert_eq!(
        read(&dir.path().join(".gemini/settings.json"))["mcpServers"]["confer"]["command"],
        "confer"
    );
}

#[test]
fn paired_hosts_install_bundled_skills_in_user_and_project_scopes() {
    for (host, root) in [
        ("gemini-cli", ".gemini"),
        ("qwen", ".qwen"),
        ("droid", ".factory"),
        ("qoder", ".qoder"),
        ("kiro", ".kiro"),
    ] {
        for scope in ["user", "project"] {
            let dir = tempfile::tempdir().unwrap();
            let relative = if host == "gemini-cli" && scope == "project" {
                ".agents"
            } else {
                root
            };
            let target = dir.path().join(relative).join("skills/confer");
            let preview = success(run(
                dir.path(),
                &[
                    "skill",
                    "install",
                    "--agent",
                    host,
                    "--scope",
                    scope,
                    "--dry-run",
                ],
            ));
            assert!(
                preview.contains(target.to_str().unwrap()),
                "{host}: {preview}"
            );
            assert!(!target.exists(), "{host}");
            success(run(
                dir.path(),
                &[
                    "skill", "install", "--agent", host, "--scope", scope, "--yes",
                ],
            ));
            assert_eq!(
                std::fs::read(target.join("SKILL.md")).unwrap(),
                include_bytes!("../skills/confer/SKILL.md"),
                "{host}: {scope}"
            );
            assert_eq!(
                std::fs::read(target.join("agents/openai.yaml")).unwrap(),
                include_bytes!("../skills/confer/agents/openai.yaml"),
                "{host}: {scope}"
            );
            for (_, config, _) in hosts() {
                assert!(!dir.path().join(config).exists());
            }
        }
    }
}

#[test]
fn skill_directory_overrides_follow_host_configuration_roots() {
    for (host, variable, value, root) in [
        ("gemini-cli", "GEMINI_CLI_HOME", "custom", "custom/.gemini"),
        ("qwen", "QWEN_HOME", "custom", "custom"),
        ("qwen", "QWEN_HOME", "~/custom", "custom"),
        ("qoder", "QODER_CONFIG_DIR", "custom", "custom"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join(root).join("skills/confer/SKILL.md");
        let preview = success(run_with_env(
            dir.path(),
            &[
                "skill",
                "install",
                "--agent",
                host,
                "--scope",
                "user",
                "--dry-run",
            ],
            &[(variable, value)],
        ));
        assert!(
            preview.contains(target.parent().unwrap().to_str().unwrap()),
            "{preview}"
        );
        assert!(!target.exists());
        success(run_with_env(
            dir.path(),
            &[
                "skill", "install", "--agent", host, "--scope", "user", "--yes",
            ],
            &[(variable, value)],
        ));
        assert_eq!(
            std::fs::read(target).unwrap(),
            include_bytes!("../skills/confer/SKILL.md")
        );
        for default in [".gemini", ".qwen", ".qoder"] {
            assert!(!dir.path().join(default).exists());
        }
    }
}

#[test]
fn external_skill_roots_fail_without_writes_and_do_not_block_other_targets() {
    let dir = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    let variables = [("QWEN_HOME", external.path().to_str().unwrap())];
    let output = run_with_env(
        dir.path(),
        &[
            "skill", "install", "--agent", "qwen", "--scope", "user", "--yes",
        ],
        &variables,
    );
    assert!(!output.status.success());
    assert!(!external.path().join("skills").exists());
    assert!(!dir.path().join(".qwen").exists());
    for (host, scope, path) in [
        ("qwen", "project", ".qwen/skills/confer/SKILL.md"),
        ("droid", "user", ".factory/skills/confer/SKILL.md"),
    ] {
        success(run_with_env(
            dir.path(),
            &[
                "skill", "install", "--agent", host, "--scope", scope, "--yes",
            ],
            &variables,
        ));
        assert!(dir.path().join(path).is_file());
    }
}

#[test]
fn auto_skill_selection_ignores_absent_external_hosts_but_validates_detected_hosts() {
    for (host, variable) in [
        ("qwen", "QWEN_HOME"),
        ("qoder", "QODER_CONFIG_DIR"),
        ("gemini-cli", "GEMINI_CLI_HOME"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let root = external.path().join("host");
        std::fs::create_dir(dir.path().join(".claude")).unwrap();
        let variables = [(variable, root.to_str().unwrap())];
        let args = ["skill", "install", "--scope", "user", "--yes"];
        success(run_with_env(dir.path(), &args, &variables));
        assert!(dir.path().join(".claude/skills/confer/SKILL.md").is_file());
        std::fs::remove_dir_all(dir.path().join(".claude/skills")).unwrap();
        std::fs::create_dir(&root).unwrap();
        if host == "gemini-cli" {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let bin = dir.path().join("bin");
                std::fs::create_dir(&bin).unwrap();
                let gemini = bin.join("gemini");
                std::fs::write(&gemini, "").unwrap();
                std::fs::set_permissions(&gemini, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            #[cfg(not(unix))]
            continue;
        }
        let output = run_with_env(dir.path(), &args, &variables);
        assert!(!output.status.success(), "{host}: {output:?}");
        assert!(!dir.path().join(".claude/skills").exists());
        assert!(!root.join("skills").exists());
        assert!(!root.join(".gemini/skills").exists());
    }
}

#[cfg(unix)]
#[test]
fn auto_skill_selection_distinguishes_gemini_from_antigravity_by_executable() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".gemini/antigravity-cli")).unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let args = ["skill", "install", "--scope", "user", "--yes"];
    let output = success(run(dir.path(), &args));
    assert!(output.contains("antigravity"), "{output}");
    assert!(!dir.path().join(".gemini/skills").exists());
    let gemini = bin.join("gemini");
    std::fs::write(&gemini, "").unwrap();
    std::fs::set_permissions(&gemini, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = success(run(dir.path(), &args));
    assert!(output.contains("gemini-cli"), "{output}");
    assert!(dir.path().join(".gemini/skills/confer/SKILL.md").is_file());
}
