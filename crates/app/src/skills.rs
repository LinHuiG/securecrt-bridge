//! Client discovery only inspects documented config roots and PATH; never runs agents.
use anyhow::{bail, ensure, Result};
use scb_core::Installation;
use serde::Serialize;
use std::path::{Component, Path, PathBuf};

pub const NAME: &str = "securecrt-bridge";
#[derive(Clone, Debug, Serialize)]
pub struct Client {
    pub name: String,
    pub directory: PathBuf,
    pub detected: bool,
    pub selected: bool,
}

pub fn path_eq(a: &Path, b: &Path) -> bool {
    let mut a = a.components();
    let mut b = b.components();
    loop {
        match (a.next(), b.next()) {
            (None, None) => return true,
            (Some(a), Some(b)) if a.as_os_str().eq_ignore_ascii_case(b.as_os_str()) => {}
            _ => return false,
        }
    }
}

fn discover_with(
    home: &Path,
    env: &dyn Fn(&str) -> Option<PathBuf>,
    on_path: &dyn Fn(&str) -> bool,
    previous: Option<&Installation>,
) -> Vec<Client> {
    let specs = [
        ("Codex", "CODEX_HOME", ".codex", "codex"),
        ("DeepSeek Harness", "DSH_HOME", ".dsh", "dsh"),
        ("Claude Code", "CLAUDE_CONFIG_DIR", ".claude", "claude"),
        ("Cursor", "", ".cursor", "cursor"),
        ("GitHub Copilot", "", ".copilot", "copilot"),
        ("Gemini CLI", "", ".gemini", "gemini"),
        ("OpenCode", "", ".config/opencode", "opencode"),
    ];
    let previous_paths = previous.map(Installation::skill_paths);
    let mut clients: Vec<Client> = specs
        .into_iter()
        .map(|(name, key, default, executable)| {
            let configured = if key.is_empty() {
                None
            } else {
                env(key).filter(|p| p.is_absolute())
            };
            let root = configured.clone().unwrap_or_else(|| {
                if name == "OpenCode" {
                    env("XDG_CONFIG_HOME")
                        .filter(|p| p.is_absolute())
                        .unwrap_or_else(|| home.join(".config"))
                        .join("opencode")
                } else {
                    home.join(default)
                }
            });
            let directory = root.join("skills").join(NAME);
            let detected = configured.is_some() || root.is_dir() || on_path(executable);
            let selected = detected
                || previous_paths
                    .as_ref()
                    .is_some_and(|dirs| dirs.iter().any(|p| path_eq(p, &directory)));
            Client {
                name: name.into(),
                directory,
                detected,
                selected,
            }
        })
        .collect();
    for directory in previous_paths.unwrap_or_default() {
        if !clients.iter().any(|c| path_eq(&c.directory, &directory)) {
            clients.push(Client {
                name: "其他客户端（上次安装）".into(),
                directory,
                detected: true,
                selected: true,
            });
        }
    }
    clients
}

pub fn discover(previous: Option<&Installation>) -> Vec<Client> {
    let home = PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default());
    let paths: Vec<_> = std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p)
                .filter(|p| p.is_absolute())
                .collect()
        })
        .unwrap_or_default();
    discover_with(
        &home,
        &|key| std::env::var_os(key).map(PathBuf::from),
        &|name| {
            paths.iter().any(|p| {
                ["exe", "cmd", "bat"]
                    .iter()
                    .any(|ext| p.join(format!("{name}.{ext}")).is_file())
            })
        },
        previous,
    )
}

pub fn normalize(paths: Vec<PathBuf>) -> Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    for path in paths {
        ensure!(
            path.is_absolute() && !path.components().any(|c| matches!(c, Component::ParentDir)),
            "Skill 路径必须是绝对目录，且不能包含 ..：{}",
            path.display()
        );
        ensure!(
            path.file_name()
                .is_some_and(|s| s.eq_ignore_ascii_case(NAME)),
            "Skill 目录最后一级必须是 {NAME}：{}",
            path.display()
        );
        ensure!(!path.is_file(), "Skill 路径不是目录：{}", path.display());
        if !result.iter().any(|p: &PathBuf| path_eq(p, &path)) {
            result.push(path);
        }
    }
    Ok(result)
}

pub fn selected(args: &[String], previous: Option<&Installation>) -> Result<Vec<PathBuf>> {
    let mut explicit = Vec::new();
    let mut mode = None;
    let mut i = 0;
    while i < args.len() {
        if matches!(args[i].as_str(), "--skills" | "--skill-dir") {
            let key = &args[i];
            i += 1;
            let value = args
                .get(i)
                .filter(|s| !s.starts_with("--"))
                .ok_or_else(|| anyhow::anyhow!("{key} 缺少参数"))?;
            if key == "--skills" {
                ensure!(mode.is_none(), "--skills 不能重复");
                mode = Some(value.as_str());
            } else {
                explicit.push(PathBuf::from(value));
            }
        }
        i += 1;
    }
    match mode {
        Some("none") => {
            ensure!(explicit.is_empty(), "--skills none 不能与 --skill-dir 同用");
            Ok(vec![])
        }
        Some("auto") | None => {
            if !explicit.is_empty() {
                normalize(explicit)
            } else {
                normalize(
                    discover(previous)
                        .into_iter()
                        .filter(|c| c.selected)
                        .map(|c| c.directory)
                        .collect(),
                )
            }
        }
        Some(other) => bail!("未知的 --skills 模式：{other}（使用 auto 或 none）"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects_only_present_clients_and_honors_dsh_home() {
        let root = std::env::temp_dir().join(format!("scb-agents-{}", scb_core::id()));
        std::fs::create_dir_all(root.join(".codex")).unwrap();
        let clients = discover_with(
            &root,
            &|k| (k == "DSH_HOME").then(|| root.join("custom-dsh")),
            &|name| name == "copilot",
            None,
        );
        let selected: Vec<_> = clients
            .iter()
            .filter(|c| c.selected)
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(selected, ["Codex", "DeepSeek Harness", "GitHub Copilot"]);
        assert_eq!(
            clients[1].directory,
            root.join("custom-dsh/skills/securecrt-bridge")
        );
        assert!(!root.join(".claude").exists());
        std::fs::remove_dir(root.join(".codex")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
    #[test]
    fn explicit_selection_skip_and_path_validation() {
        let args = |s: &[&str]| s.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(selected(&args(&["--skills", "none"]), None)
            .unwrap()
            .is_empty());
        let paths = selected(
            &args(&[
                "--skill-dir",
                "C:/Agents/skills/securecrt-bridge",
                "--skill-dir",
                "c:\\agents\\skills\\securecrt-bridge",
            ]),
            None,
        )
        .unwrap();
        assert_eq!(paths.len(), 1);
        for s in [
            vec!["--skill-dir"],
            vec!["--skills", "bad"],
            vec![
                "--skills",
                "none",
                "--skill-dir",
                "C:/skills/securecrt-bridge",
            ],
            vec!["--skill-dir", "relative/securecrt-bridge"],
            vec!["--skill-dir", "C:/skills/../securecrt-bridge"],
        ] {
            assert!(selected(&args(&s), None).is_err());
        }
    }
    #[test]
    fn upgrades_preserve_selection_and_custom_paths() {
        let mut old: Installation = serde_json::from_value(serde_json::json!({"version":"0.1.6","install_dir":"C:/Bridge","securecrt_exe":"C:/SecureCRT.exe","launcher":"C:/SecureCRT.exe","config_dir":"C:/Config","skill_dir":"C:/legacy/securecrt-bridge"})).unwrap();
        let clients = discover_with(Path::new("C:/Example"), &|_| None, &|_| false, Some(&old));
        assert_eq!(clients.iter().filter(|c| c.selected).count(), 1);
        assert_eq!(clients.last().unwrap().directory, old.skill_dir);
        old.skill_dirs = Some(vec![]);
        assert!(
            discover_with(Path::new("C:/Example"), &|_| None, &|_| false, Some(&old))
                .iter()
                .all(|c| !c.selected)
        );
    }
}
