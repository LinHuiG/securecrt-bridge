use crate::{option, platform};
use anyhow::{bail, Context, Result};
use scb_core::{
    config::{self, Change},
    w, Installation,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use windows::{
    core::PCWSTR,
    Win32::{
        System::{
            Com::CLSIDFromProgID,
            Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE},
        },
        UI::WindowsAndMessaging::*,
    },
};
const SCRIPT_KEY: &str = "S:\"Script Filename V2\"";
const USE_KEY: &str = "D:\"Use Script File\"";
const SKILL: &str = include_str!("../../../assets/skill/SKILL.md");
#[derive(Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub launcher: PathBuf,
    pub executable: PathBuf,
    pub config: PathBuf,
}
#[derive(Serialize, Deserialize, Default)]
struct Journal {
    changes: Vec<Change>,
    registry: Vec<RegChange>,
    files: BTreeMap<PathBuf, String>,
    skipped: Vec<String>,
}
#[derive(Serialize, Deserialize)]
struct RegChange {
    key: String,
    name: String,
    before: Option<String>,
    after: String,
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn same_path(a: &Path, b: &Path) -> bool {
    let mut left = a.components();
    let mut right = b.components();
    loop {
        match (left.next(), right.next()) {
            (None, None) => return true,
            (Some(x), Some(y)) if x.as_os_str().eq_ignore_ascii_case(y.as_os_str()) => {}
            _ => return false,
        }
    }
}
fn default_skill() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()).join(".codex")
        })
        .join("skills/securecrt-bridge")
}
pub fn candidate(path: &Path) -> Option<Candidate> {
    let p = if path.is_dir() {
        if path.join("SecureCRTPortable.exe").exists() {
            path.join("SecureCRTPortable.exe")
        } else {
            path.join("SecureCRT.exe")
        }
    } else {
        path.into()
    };
    let name = p.file_name()?.to_string_lossy();
    let parent = p.parent()?;
    if name.eq_ignore_ascii_case("SecureCRTPortable.exe")
        && parent.join("App/Clients/SecureCRT.exe").exists()
    {
        return Some(Candidate {
            launcher: p.clone(),
            executable: parent.join("App/Clients/SecureCRT.exe"),
            config: parent.join("Data/Config"),
        });
    }
    if !name.eq_ignore_ascii_case("SecureCRT.exe") || !p.exists() {
        return None;
    }
    if let Some(root) = parent.parent().and_then(Path::parent) {
        if root.join("SecureCRTPortable.exe").exists() {
            return candidate(&root.join("SecureCRTPortable.exe"));
        }
    }
    let config = platform::reg_get(
        HKEY_CURRENT_USER,
        "Software\\VanDyke\\SecureCRT",
        "Config Path",
    )
    .map(PathBuf::from)
    .unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("VanDyke/Config")
    });
    Some(Candidate {
        launcher: p.clone(),
        executable: p,
        config,
    })
}
pub fn detect() -> Vec<Candidate> {
    let mut paths = Vec::new();
    if let Ok(i) = Installation::load() {
        paths.push(i.launcher);
    }
    for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        for key in [
            "Software\\Microsoft\\Windows\\CurrentVersion\\App Paths\\SecureCRT.exe",
            "Software\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\App Paths\\SecureCRT.exe",
        ] {
            if let Some(p) = platform::reg_get(root, key, "") {
                paths.push(PathBuf::from(p.trim_matches('"')));
            }
        }
    }
    for env in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(p) = std::env::var_os(env) {
            paths.push(PathBuf::from(&p).join("VanDyke Software/SecureCRT/SecureCRT.exe"));
            paths.push(PathBuf::from(p).join("VanDyke Software/Clients/SecureCRT.exe"));
        }
    }
    let roots = [
        PathBuf::from("D:/WorkSpace/Soft/Tools"),
        PathBuf::from("D:/Tools"),
        PathBuf::from("C:/Tools"),
        PathBuf::from("C:/Program Files"),
        PathBuf::from("C:/Program Files (x86)"),
    ];
    let until = Instant::now() + Duration::from_secs(4);
    let mut stack: Vec<_> = roots.into_iter().map(|p| (p, 0)).collect();
    let mut seen = 0;
    while let Some((dir, depth)) = stack.pop() {
        if seen > 30000 || Instant::now() > until {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            seen += 1;
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_lowercase();
            if name == "securecrt.exe" || name == "securecrtportable.exe" {
                paths.push(p.clone());
            }
            if depth < 5
                && e.file_type().is_ok_and(|f| f.is_dir() && !f.is_symlink())
                && !matches!(
                    name.as_str(),
                    "node_modules" | ".git" | "target" | "windows" | ".build"
                )
            {
                stack.push((p, depth + 1));
            }
        }
    }
    let mut result = BTreeMap::new();
    for p in paths {
        if let Some(c) = candidate(&p) {
            result.insert(c.executable.to_string_lossy().to_lowercase(), c);
        }
    }
    result.into_values().collect()
}
fn resolve(args: &[String], repair: bool) -> Result<Candidate> {
    let noninteractive = args.iter().any(|s| s == "--noninteractive");
    let mut found = if let Some(p) = option(args, "--path") {
        candidate(Path::new(p)).context("Selected path is not a SecureCRT installation")?
    } else if repair {
        let i = Installation::load()?;
        Candidate {
            launcher: i.launcher,
            executable: i.securecrt_exe,
            config: i.config_dir,
        }
    } else {
        if noninteractive {
            bail!("请选择 SecureCRT 安装目录。");
        }
        let candidates = detect();
        let mut picked = None;
        for c in candidates {
            let text = format!(
                "安装到此 SecureCRT？\n\n{}\n\n配置目录：{}\n\n选择“否”继续查找；“取消”退出。",
                c.launcher.display(),
                c.config.display()
            );
            match unsafe {
                MessageBoxW(
                    None,
                    PCWSTR(w(text).as_ptr()),
                    PCWSTR(w("SecureCRT Bridge 安装").as_ptr()),
                    MB_YESNOCANCEL | MB_ICONQUESTION,
                )
            } {
                IDYES => {
                    picked = Some(c);
                    break;
                }
                IDCANCEL => bail!("Installation cancelled"),
                _ => {}
            }
        }
        if let Some(c) = picked {
            c
        } else {
            candidate(&platform::choose_file(
                "选择 SecureCRT.exe 或 SecureCRTPortable.exe",
                "SecureCRT.exe",
            )?)
            .context("Invalid SecureCRT path")?
        }
    };
    if let Some(p) = option(args, "--config") {
        found.config = p.into();
    }
    if !found.config.join("Sessions/Default.ini").exists() {
        if noninteractive {
            bail!("配置目录中找不到 Sessions\\Default.ini，请选择正确的配置目录。");
        }
        let ini = platform::choose_file("选择配置目录中的 Sessions\\Default.ini", "Default.ini")?;
        if ini
            .file_name()
            .is_none_or(|n| !n.eq_ignore_ascii_case("Default.ini"))
        {
            bail!("Select Sessions/Default.ini");
        }
        found.config = ini
            .parent()
            .and_then(Path::parent)
            .context("Invalid config path")?
            .into();
    }
    platform::validate_exe(&found.executable)?;
    Ok(found)
}
fn journal_path() -> PathBuf {
    scb_core::data_dir().join("install-journal.json")
}
fn save(j: &Journal) -> Result<()> {
    scb_core::save_json(&journal_path(), j)
}
fn write_owned(j: &mut Journal, path: &Path, bytes: &[u8]) -> Result<()> {
    if path.exists() {
        let old = std::fs::read(path)?;
        if old == bytes {
            j.files.insert(path.into(), hash(bytes));
            return save(j);
        }
        if j.files.get(path) != Some(&hash(&old)) {
            bail!(
                "Refusing to overwrite a file modified outside Bridge: {}",
                path.display()
            );
        }
    }
    scb_core::atomic_write(path, bytes)?;
    j.files.insert(path.into(), hash(bytes));
    save(j)
}
fn set_reg(j: &mut Journal, key: &str, name: &str, value: &str) -> Result<()> {
    if let Some(old) = j
        .registry
        .iter_mut()
        .find(|r| r.key == key && r.name == name)
    {
        let current = platform::reg_get(HKEY_CURRENT_USER, key, name);
        if current.as_deref() != Some(&old.after) && current != old.before {
            bail!("Registry value changed outside Bridge: {key} {name}");
        }
        old.after = value.into();
    } else {
        j.registry.push(RegChange {
            key: key.into(),
            name: name.into(),
            before: platform::reg_get(HKEY_CURRENT_USER, key, name),
            after: value.into(),
        });
    }
    save(j)?;
    platform::reg_set(key, name, Some(value))
}
fn session_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut stack = vec![root.into()];
    let mut files = Vec::new();
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(dir)?.flatten() {
            let p = e.path();
            let ft = e.file_type()?;
            if ft.is_dir() && !ft.is_symlink() {
                stack.push(p);
            } else if p.extension().is_some_and(|s| s.eq_ignore_ascii_case("ini"))
                && !p
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("__"))
            {
                files.push(p);
            }
        }
    }
    Ok(files)
}
fn hook(j: &mut Journal, file: &Path, install: &Installation) -> Result<()> {
    let (text, _) = config::decode(&std::fs::read(file)?)?;
    let current = config::values(&text);
    let protocol = current
        .get("S:\"Protocol Name\"")
        .map(String::as_str)
        .unwrap_or("SSH2");
    if !matches!(protocol, "SSH2" | "SSH1" | "Telnet") {
        return Ok(());
    }
    if let Some(previous) = j.changes.iter().find(|c| c.path == file) {
        if config::reapply(previous)? {
            return Ok(());
        }
        j.skipped.push(format!(
            "{}: Bridge hook was edited by user; left unchanged",
            file.display()
        ));
        return Ok(());
    }
    let old_script = current.get(SCRIPT_KEY).cloned().unwrap_or_default();
    let enabled = current
        .get(USE_KEY)
        .is_some_and(|s| s != "00000000" && s != "0");
    let previous = if enabled && !old_script.is_empty() {
        let bytes = std::fs::read(&old_script)
            .with_context(|| format!("Read existing script: {old_script}"))?;
        let (script, _) = config::decode(&bytes)?;
        let lang = script
            .lines()
            .find(|l| l.trim_start().starts_with("#$language"))
            .and_then(|l| l.split('"').nth(1))
            .unwrap_or("");
        if lang == scb_core::ENGINE_PROGID
            || lang.is_empty()
            || unsafe { CLSIDFromProgID(PCWSTR(w(lang).as_ptr())) }.is_err()
        {
            j.skipped.push(format!(
                "{}: Existing script language cannot be chained via ActiveX; kept unchanged",
                file.display()
            ));
            return Ok(());
        }
        old_script
    } else {
        String::new()
    };
    let descriptor = install.install_dir.join("hooks").join(format!(
        "{}.scb",
        &hash(file.to_string_lossy().as_bytes())[..24]
    ));
    let entry = format!(
        "#$language = \"{}\"\r\n#$interface = \"1.0\"\r\n{}\r\n",
        scb_core::ENGINE_PROGID,
        json!({"previous_script":previous})
    );
    write_owned(j, &descriptor, entry.as_bytes())?;
    let updates: BTreeMap<String, String> = BTreeMap::from([
        (USE_KEY.into(), "00000001".into()),
        (SCRIPT_KEY.into(), descriptor.to_string_lossy().to_string()),
    ]);
    let original = updates
        .keys()
        .map(|key| (key.clone(), current.get(key).cloned()))
        .collect();
    // Journal before mutation makes interrupted installs recoverable.
    let change = Change {
        path: file.into(),
        original,
        installed: updates.clone(),
    };
    j.changes.push(change);
    save(j)?;
    config::install_patch(file, updates, &scb_core::data_dir().join("backups"))?;
    Ok(())
}
pub fn install(args: &[String], repair: bool) -> Result<Value> {
    install_with_progress(args, repair, &|_, _| {})
}
pub fn install_with_progress(
    args: &[String],
    repair: bool,
    progress: &dyn Fn(u32, &str),
) -> Result<Value> {
    progress(0, "检查安装包、路径和 SecureCRT 状态");
    if crate::ENGINE.is_empty() {
        bail!("Development binary has no bundled engine; run build.ps1 release");
    }
    let mut c = resolve(args, repair)?;
    if !platform::securecrt_processes()?.is_empty() {
        bail!("请先关闭 SecureCRT（避免它退出时覆盖登录配置），再运行安装器。现有 SSH 会话没有被关闭或修改。");
    }
    let root = c.launcher.parent().context("Invalid install path")?;
    let mut dir = root.join("SecureCRTBridge");
    let previous_install = Installation::load().ok();
    if let Some(old) = previous_install.as_ref() {
        if !same_path(&old.install_dir, &dir) || !same_path(&old.config_dir, &c.config) {
            bail!("A different SecureCRT installation or configuration is registered. Uninstall it before changing installation location.");
        }
        // Windows discovery may return different casing. Keep recorded spellings
        // so owned-file hashes and original configuration journal stay continuous.
        dir = old.install_dir.clone();
        c.launcher = old.launcher.clone();
        c.executable = old.securecrt_exe.clone();
        c.config = old.config_dir.clone();
    }
    platform::private_directory(&scb_core::data_dir())?;
    progress(10, "路径和权限检查通过，准备停止旧服务");
    // Older brokers incorrectly persist paused=true on shutdown. Preserve the
    // live user choice across upgrades instead of importing that shutdown side effect.
    let previous_pause = crate::local("GET", "/health", &json!({}))
        .ok()
        .and_then(|v| v["paused"].as_bool());
    let _ = std::fs::write(scb_core::data_dir().join("stopped"), b"install");
    let _ = crate::local("POST", "/shutdown", &json!({}));
    for _ in 0..30 {
        if crate::local("GET", "/health", &json!({})).is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_millis(700));
    if let Some(paused) = previous_pause {
        scb_core::save_json(
            &scb_core::data_dir().join("preferences.json"),
            &json!({"paused":paused}),
        )?;
    }
    progress(20, "旧服务已停止，读取安装记录并保留原配置备份");
    let mut j: Journal = if journal_path().exists() {
        scb_core::load_json(&journal_path())?
    } else {
        Journal::default()
    };
    j.skipped.clear();
    std::fs::create_dir_all(&dir)?;
    let install = Installation {
        version: scb_core::VERSION.into(),
        install_dir: dir.clone(),
        securecrt_exe: c.executable,
        launcher: c.launcher,
        config_dir: c.config,
        skill_dir: default_skill(),
    };
    // Save discovery early so repair/uninstall can recover a partial install.
    scb_core::save_json(&scb_core::data_dir().join("installation.json"), &install)?;
    write_owned(&mut j, &dir.join("scb_engine.dll"), crate::ENGINE)?;
    progress(32, "终端接入模块已写入");
    let exe = std::env::current_exe()?;
    let bytes = std::fs::read(&exe)?;
    write_owned(&mut j, &dir.join("securecrt-bridge.exe"), &bytes)?;
    progress(43, "本地服务和命令行程序已写入");
    write_owned(
        &mut j,
        &install.skill_dir.join("SKILL.md"),
        SKILL.as_bytes(),
    )?;
    write_owned(
        &mut j,
        &dir.join("README.txt"),
        include_bytes!("../../../assets/README.txt"),
    )?;
    write_owned(
        &mut j,
        &dir.join("LICENSE.txt"),
        include_bytes!("../../../LICENSE"),
    )?;
    write_owned(
        &mut j,
        &dir.join("THIRD_PARTY_LICENSES.txt"),
        include_bytes!("../../../assets/THIRD_PARTY_LICENSES.txt"),
    )?;
    progress(
        52,
        &format!(
            "AI skill 已安装：{}",
            install.skill_dir.join("SKILL.md").display()
        ),
    );
    progress(
        54,
        &format!("程序安装目录：{}", install.install_dir.display()),
    );
    progress(
        56,
        &format!("SecureCRT 配置目录：{}", install.config_dir.display()),
    );
    let cls = format!("Software\\Classes\\CLSID\\{}", scb_core::ENGINE_CLSID);
    set_reg(&mut j, &cls, "", "SecureCRT Bridge Native Scripting Engine")?;
    set_reg(
        &mut j,
        &format!("{cls}\\InprocServer32"),
        "",
        &dir.join("scb_engine.dll").to_string_lossy(),
    )?;
    set_reg(
        &mut j,
        &format!("{cls}\\InprocServer32"),
        "ThreadingModel",
        "Apartment",
    )?;
    set_reg(
        &mut j,
        &format!("{cls}\\ProgID"),
        "",
        scb_core::ENGINE_PROGID,
    )?;
    for cat in [
        "{F0B7A1A1-9847-11CF-8F20-00805F2CD064}",
        "{F0B7A1A2-9847-11CF-8F20-00805F2CD064}",
    ] {
        set_reg(
            &mut j,
            &format!("{cls}\\Implemented Categories\\{cat}"),
            "",
            "",
        )?;
    }
    let prog = format!("Software\\Classes\\{}", scb_core::ENGINE_PROGID);
    set_reg(&mut j, &prog, "", "SecureCRT Bridge Native")?;
    set_reg(
        &mut j,
        &format!("{prog}\\CLSID"),
        "",
        scb_core::ENGINE_CLSID,
    )?;
    set_reg(&mut j, &format!("{prog}\\OLEScript"), "", "")?;
    progress(62, "终端接入组件已注册，正在配置自动接入");
    let files = session_files(&install.config_dir.join("Sessions"))?;
    let count = files.len().max(1);
    for (i, file) in files.iter().enumerate() {
        hook(&mut j, file, &install)?;
        progress(
            62 + (i + 1) as u32 * 20 / count as u32,
            &format!(
                "已处理会话配置 {}/{}：{}",
                i + 1,
                files.len(),
                file.file_name().unwrap_or_default().to_string_lossy()
            ),
        );
    }
    let exe = dir.join("securecrt-bridge.exe");
    let startup = format!("\"{}\" serve", exe.display());
    set_reg(
        &mut j,
        "Software\\Microsoft\\Windows\\CurrentVersion\\Run",
        "SecureCRTBridge",
        &startup,
    )?;
    let uninst = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\SecureCRTBridge";
    for (name, value) in [
        ("DisplayName", "SecureCRT Bridge".to_owned()),
        ("DisplayVersion", scb_core::VERSION.into()),
        ("InstallLocation", dir.to_string_lossy().to_string()),
        (
            "UninstallString",
            format!("\"{}\" uninstall --gui", exe.display()),
        ),
    ] {
        set_reg(&mut j, uninst, name, &value)?;
    }
    progress(90, "自动启动和卸载信息已设置");
    progress(
        92,
        &format!(
            "原配置备份目录：{}",
            scb_core::data_dir().join("backups").display()
        ),
    );
    save(&j)?;
    progress(95, "正在启动并检查本地服务");
    crate::start()?;
    for skipped in &j.skipped {
        progress(98, &format!("保留原配置：{skipped}"));
    }
    progress(100, "安装完成，可以打开 SecureCRT 或从齐治连接 SSH");
    if args.is_empty() || args.iter().any(|s| s == "--gui") {
        platform::message(&format!("安装完成。\n\n现在打开 SecureCRT 或从齐治点击 SSH，即可自动接入。\n托盘菜单可暂停、恢复或退出。\n\n配置已备份，skill 已安装。\n跳过的配置数：{}",j.skipped.len()),false);
    }
    Ok(
        json!({"ok":true,"installation":install,"hooked_sessions":j.changes.len(),"skipped":j.skipped}),
    )
}
pub fn uninstall(args: &[String]) -> Result<Value> {
    uninstall_with_progress(args, &|_, _| {})
}
pub fn uninstall_with_progress(args: &[String], progress: &dyn Fn(u32, &str)) -> Result<Value> {
    progress(0, "检查已安装版本和 SecureCRT 状态");
    let install = Installation::load()?;
    if !platform::securecrt_processes()?.is_empty() {
        bail!("请先关闭 SecureCRT，再卸载桥接模块。");
    }
    let mut conflicts = Vec::new();
    let j: Journal = scb_core::load_json(&journal_path())?;
    progress(10, "正在停止本地服务");
    std::fs::write(scb_core::data_dir().join("stopped"), b"uninstalled")?;
    let _ = crate::local("POST", "/shutdown", &json!({}));
    std::thread::sleep(Duration::from_millis(600));
    progress(25, "正在恢复原来的会话登录配置");
    for change in &j.changes {
        if change.path.exists() {
            for key in config::restore(change)? {
                conflicts.push(format!("{}: {key}", change.path.display()));
            }
        }
    }
    progress(45, "正在移除接入注册、启动项和卸载信息");
    for r in j.registry.iter().rev() {
        let current = platform::reg_get(HKEY_CURRENT_USER, &r.key, &r.name);
        if current.as_deref() == Some(&r.after) {
            platform::reg_set(&r.key, &r.name, r.before.as_deref())?;
        } else if current != r.before {
            conflicts.push(format!("Registry: {} {}", r.key, r.name));
        }
    }
    progress(65, "正在移除程序和 AI skill，保留用户修改的文件");
    // Never recursively delete an installation tree or remove user-edited files.
    for (path, expected) in &j.files {
        if let Ok(bytes) = std::fs::read(path) {
            if hash(&bytes) == *expected {
                if std::fs::remove_file(path).is_err() {
                    conflicts.push(format!(
                        "Still in use; remove after exit: {}",
                        path.display()
                    ));
                }
            } else {
                conflicts.push(format!("User-modified file retained: {}", path.display()));
            }
        }
    }
    let _ = std::fs::remove_dir(install.install_dir.join("hooks"));
    let _ = std::fs::remove_dir(&install.install_dir);
    let _ = std::fs::remove_dir(&install.skill_dir);
    let _ = std::fs::remove_file(scb_core::data_dir().join("connection.json"));
    let _ = std::fs::remove_file(scb_core::data_dir().join("installation.json"));
    // Retain the journal/backups for recovery; a subsequent install starts a new journal.
    let archived = scb_core::data_dir().join(format!("uninstalled-{}.json", scb_core::now_ms()));
    std::fs::rename(journal_path(), archived)?;
    for conflict in &conflicts {
        progress(95, &format!("保留项：{conflict}"));
    }
    progress(100, "卸载完成，原配置备份已保留");
    if args.iter().any(|s| s == "--gui") {
        platform::message(
            &format!(
                "自动接入、启动项和 skill 已卸载。\n原配置备份保留在：{}\n\n保留项：{}",
                scb_core::data_dir().display(),
                conflicts.join("\n")
            ),
            false,
        );
    }
    Ok(json!({"ok":true,"retained":conflicts,"backups":scb_core::data_dir().join("backups")}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects_portable_layout() {
        let root = std::env::temp_dir().join(format!("scb-layout-{}", scb_core::id()));
        std::fs::create_dir_all(root.join("App/Clients")).unwrap();
        std::fs::write(root.join("SecureCRTPortable.exe"), []).unwrap();
        std::fs::write(root.join("App/Clients/SecureCRT.exe"), []).unwrap();
        let c = candidate(&root).unwrap();
        assert!(c.executable.ends_with("App/Clients/SecureCRT.exe"));
        assert!(c.config.ends_with("Data/Config"));
        assert!(same_path(
            &candidate(&c.executable).unwrap().launcher,
            &c.launcher
        ));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn upgrade_path_matching_ignores_windows_case_and_separators() {
        assert!(same_path(
            Path::new("d:\\workspace\\SecureCRT\\Data/Config"),
            Path::new("D:/WorkSpace/SecureCRT/Data/Config")
        ));
        assert!(!same_path(
            Path::new("D:/SecureCRT"),
            Path::new("D:/SecureCRT2")
        ));
    }
}
