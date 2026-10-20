//! The NSIS wizard passes a UTF-16 INI plan, never interpolated shell commands.
use crate::{installer, option, skills};
use anyhow::{ensure, Context, Result};
use scb_core::{config, Installation};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

type Plan = BTreeMap<(String, String), String>;
fn read(path: &Path) -> Result<Plan> {
    let bytes = std::fs::read(path)?;
    ensure!(bytes.len() <= 262_144, "Setup plan too large");
    let (text, _) = config::decode(&bytes)?;
    let mut section = String::new();
    let mut plan = Plan::new();
    for line in text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with(';'))
    {
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].into();
        } else {
            let (key, value) = line.split_once('=').context("Invalid setup plan")?;
            let value = value.trim().trim_matches('"');
            ensure!(
                plan.insert((section.clone(), key.trim().into()), value.into())
                    .is_none(),
                "Duplicate setup option"
            );
        }
    }
    Ok(plan)
}
fn get<'a>(p: &'a Plan, section: &str, key: &str) -> &'a str {
    p.get(&(section.into(), key.into()))
        .map(String::as_str)
        .unwrap_or("")
}
fn put(p: &mut Plan, section: &str, key: &str, value: impl ToString) -> Result<()> {
    let value = value.to_string();
    ensure!(
        !value.contains(['\r', '\n', '\0', '"']),
        "Invalid setup value"
    );
    p.insert((section.into(), key.into()), value);
    Ok(())
}
fn write(path: &Path, plan: &Plan) -> Result<()> {
    let mut text = String::new();
    let mut previous = "";
    for ((section, key), value) in plan {
        if section != previous {
            text += &format!("[{section}]\r\n");
            previous = section;
        }
        text += &format!("{key}={value}\r\n");
    }
    scb_core::atomic_write(path, &config::encode(&text, true))
}
pub fn default_directory(previous: Option<&Installation>) -> PathBuf {
    previous.map(|p| p.install_dir.clone()).unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default())
            .join("AppData/Local/Programs/SecureCRTBridge")
    })
}
pub fn export(path: &Path) -> Result<Value> {
    let previous = Installation::load().ok();
    let candidates = installer::detect();
    let mut plan = Plan::new();
    put(
        &mut plan,
        "setup",
        "directory",
        default_directory(previous.as_ref()).display(),
    )?;
    put(
        &mut plan,
        "setup",
        "existing",
        previous.as_ref().map(|p| p.version.as_str()).unwrap_or(""),
    )?;
    let candidate = candidates.first();
    put(
        &mut plan,
        "setup",
        "securecrt",
        previous
            .as_ref()
            .map(|p| p.launcher.display().to_string())
            .or_else(|| candidate.map(|c| c.launcher.display().to_string()))
            .unwrap_or_default(),
    )?;
    put(
        &mut plan,
        "setup",
        "config",
        previous
            .as_ref()
            .map(|p| p.config_dir.display().to_string())
            .or_else(|| candidate.map(|c| c.config.display().to_string()))
            .unwrap_or_default(),
    )?;
    let clients = skills::discover(previous.as_ref());
    put(&mut plan, "setup", "clients", clients.len())?;
    for (i, client) in clients.iter().enumerate() {
        let section = format!("client{i}");
        put(&mut plan, &section, "name", &client.name)?;
        put(&mut plan, &section, "path", client.directory.display())?;
        put(
            &mut plan,
            &section,
            "selected",
            if client.selected { 1 } else { 0 },
        )?;
    }
    write(path, &plan)?;
    Ok(json!({"ok":true}))
}
fn arguments(plan: &Plan) -> Result<Vec<String>> {
    let mut args = vec!["install".into(), "--noninteractive".into()];
    for (key, option) in [
        ("directory", "--install-dir"),
        ("securecrt", "--path"),
        ("config", "--config"),
        ("uninstaller", "--external-uninstaller"),
    ] {
        let value = get(plan, "setup", key);
        ensure!(
            !value.is_empty() && !value.contains(['\r', '\n', '\0', '"']),
            "Missing/invalid setup {key}"
        );
        args.extend([option.into(), value.into()]);
    }
    let count: usize = get(plan, "setup", "clients").parse()?;
    ensure!(count <= 100, "Too many skill destinations");
    let mut selected = 0;
    for i in 0..count {
        let section = format!("client{i}");
        if get(plan, &section, "selected") == "1" {
            args.extend(["--skill-dir".into(), get(plan, &section, "path").into()]);
            selected += 1;
        }
    }
    let extra = get(plan, "setup", "extra_file");
    if !extra.is_empty() {
        let bytes = std::fs::read(extra)?;
        ensure!(bytes.len() <= 32_768, "Additional skill paths too large");
        let (text, _) = config::decode(&bytes)?;
        for path in text.lines().map(str::trim).filter(|s| !s.is_empty()) {
            args.extend(["--skill-dir".into(), path.into()]);
            selected += 1;
        }
    }
    if selected == 0 {
        args.extend(["--skills".into(), "none".into()]);
    }
    Ok(args)
}
pub fn run(command: &str, args: &[String]) -> Result<Value> {
    let nsis = args.iter().any(|a| a == "--nsis");
    let file = Path::new(option(args, "--file").context("Provide --file PLAN.ini")?);
    if command == "setup-export" {
        return export(file);
    }
    let args = arguments(&read(file)?)?;
    if command == "setup-validate" {
        installer::validate(&args)?;
        return Ok(json!({"ok":true}));
    }
    installer::install_with_progress(&args, false, &|percent, message| {
        if nsis {
            log(&format!("[{percent:3}%] {message}"));
        } else {
            println!("[{percent:3}%] {message}");
        }
    })
}

pub fn log(message: &str) {
    use std::io::Write;
    // nsExec recognizes UTF-16LE, but decodes UTF-8 as the Windows ANSI codepage.
    let bytes: Vec<u8> = format!("{message}\r\n")
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(&bytes);
    let _ = out.flush();
}
pub fn entry(args: &[String]) -> Result<()> {
    let command = args.first().map(String::as_str).unwrap_or("");
    if command == "setup-check" {
        ensure!(
            crate::platform::securecrt_processes()?.is_empty(),
            "请先关闭 SecureCRT，再继续安装。现有 SSH 会话不会被安装器关闭。"
        );
    } else if command == "uninstall" {
        installer::uninstall_with_progress(args, &|n, s| log(&format!("[{n:3}%] {s}")))?;
    } else {
        ensure!(
            matches!(command, "setup-export" | "setup-validate" | "setup-apply"),
            "Unsupported setup operation"
        );
        run(command, args)?;
    }
    log("OK: 操作完成");
    Ok(())
}

pub fn validate_directory(path: &Path) -> Result<()> {
    use std::path::Component;
    ensure!(
        path.is_absolute()
            && path.parent().is_some()
            && !path.components().any(|c| matches!(c, Component::ParentDir)),
        "安装目录必须为绝对子目录，且不能包含 .."
    );
    ensure!(
        !path.to_string_lossy().contains(['"', '\r', '\n', '\0']),
        "安装目录包含无效字符"
    );
    Ok(())
}
pub fn validate_uninstaller(args: &[String]) -> Result<()> {
    if let Some(path) = option(args, "--external-uninstaller") {
        let path = Path::new(path);
        validate_directory(path)?;
        let bytes = std::fs::read(path)?;
        ensure!(bytes.starts_with(b"MZ"), "Invalid uninstaller executable");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wizard_plan_preserves_unicode_spaces_and_explicit_skip() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../target/plan-{}.ini", scb_core::id()));
        let mut p = Plan::new();
        for (key, value) in [
            ("directory", "D:\\工具 = 测试\\桥接"),
            ("securecrt", "D:\\SecureCRT.exe"),
            ("config", "D:\\配置"),
            ("uninstaller", "D:\\Uninstall.exe"),
            ("clients", "1"),
        ] {
            put(&mut p, "setup", key, value).unwrap();
        }
        put(&mut p, "client0", "selected", "0").unwrap();
        write(&path, &p).unwrap();
        let args = arguments(&read(&path).unwrap()).unwrap();
        assert_eq!(
            option(&args, "--install-dir"),
            Some("D:\\工具 = 测试\\桥接")
        );
        assert_eq!(option(&args, "--skills"), Some("none"));
        assert!(validate_directory(Path::new("D:\\")).is_err());
        assert!(validate_directory(Path::new("D:\\bridge\\..\\other")).is_err());
        assert!(validate_directory(Path::new("relative")).is_err());
        std::fs::remove_file(path).unwrap();
    }
}
