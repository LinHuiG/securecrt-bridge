#![windows_subsystem = "windows"]
//! Standalone isolated host probe. Never calls the installer or modifies an existing session.
#[allow(dead_code)]
mod platform;
use anyhow::{bail, Context, Result};
use scb_core::{w, Discovery, Installation};
use serde_json::{json, Value};
use std::{
    net::{Ipv4Addr, TcpListener},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use windows::{
    core::{PCWSTR, PWSTR},
    Win32::{
        Foundation::*,
        System::{Registry::*, Threading::*},
    },
};
include!(concat!(env!("OUT_DIR"), "/payload.rs"));
const PROG: &str = "SecureCRTBridge.Diagnostic";
const CLSID: &str = "{72599488-7997-4D79-9EF6-40130489082B}";

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let headless = args.iter().any(|a| a == "--headless");
    match run() {
        Ok(result) => {
            println!("{}", result);
            if !headless {
                platform::message(&format!("隔离诊断已结束。\n\n报告目录：{}\n结果：{}\n\n仅使用临时配置和本机模拟终端，没有修改现有登录配置或发送远程命令。",result["directory"].as_str().unwrap_or(""),if result["checks_passed"]==true{"全部检查通过"}else{"存在未通过项，请提供报告"}),false);
            }
        }
        Err(e) => {
            eprintln!("{e:#}");
            if !headless {
                platform::message(&format!("诊断未完成：{e:#}"), true);
            }
            std::process::exit(1);
        }
    }
}
struct Registration {
    keys: Vec<String>,
}
impl Registration {
    fn create(dll: &Path) -> Result<Self> {
        let class = format!("Software\\Classes\\CLSID\\{CLSID}");
        let prog = format!("Software\\Classes\\{PROG}");
        for key in [&class, &prog] {
            let mut h = HKEY::default();
            let status = unsafe {
                RegOpenKeyExW(
                    HKEY_CURRENT_USER,
                    PCWSTR(w(key).as_ptr()),
                    None,
                    KEY_READ,
                    &mut h,
                )
            };
            if status == ERROR_SUCCESS {
                unsafe {
                    let _ = RegCloseKey(h);
                }
                bail!("Diagnostic registration already exists; left unchanged: {key}");
            }
            if status != ERROR_FILE_NOT_FOUND {
                status.ok()?;
            }
        }
        let registration = Self {
            keys: vec![class.clone(), prog.clone()],
        };
        platform::reg_set(&class, "", Some("SecureCRT isolated diagnostic"))?;
        platform::reg_set(
            &format!("{class}\\InprocServer32"),
            "",
            Some(&dll.to_string_lossy()),
        )?;
        platform::reg_set(
            &format!("{class}\\InprocServer32"),
            "ThreadingModel",
            Some("Apartment"),
        )?;
        platform::reg_set(&format!("{prog}\\CLSID"), "", Some(CLSID))?;
        platform::reg_set(&format!("{prog}\\OLEScript"), "", Some(""))?;
        Ok(registration)
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        for key in self.keys.iter().rev() {
            unsafe {
                let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(w(key).as_ptr()));
            }
        }
    }
}
fn quote(arg: &str) -> String {
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in arg.chars() {
        if c == '\\' {
            slashes += 1;
        } else {
            if c == '"' {
                out.push_str(&"\\".repeat(slashes * 2 + 1));
            } else {
                out.push_str(&"\\".repeat(slashes));
            }
            slashes = 0;
            out.push(c);
        }
    }
    out.push_str(&"\\".repeat(slashes * 2));
    out.push('"');
    out
}
fn spawn_isolated(exe: &Path, config: &Path, extra: &[String]) -> Result<platform::Handle> {
    unsafe {
        // No /T and Single Instance=0: this invocation owns a separate process.
        let mut args = vec![
            exe.to_string_lossy().to_string(),
            "/F".into(),
            config.to_string_lossy().to_string(),
        ];
        args.extend_from_slice(extra);
        let command = args.iter().map(|s| quote(s)).collect::<Vec<_>>().join(" ");
        let mut command = w(command);
        let mut startup = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            dwFlags: STARTF_USESHOWWINDOW,
            wShowWindow: 0,
            ..Default::default()
        };
        let mut info = PROCESS_INFORMATION::default();
        CreateProcessW(
            PCWSTR(w(exe).as_ptr()),
            Some(PWSTR(command.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_NO_WINDOW,
            None,
            PCWSTR::null(),
            &mut startup,
            &mut info,
        )?;
        let _ = CloseHandle(info.hThread);
        Ok(platform::Handle(info.hProcess))
    }
}
fn display_server(listener: TcpListener, stop: Arc<AtomicBool>) -> Result<()> {
    use std::io::Write;
    listener.set_nonblocking(true)?;
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((mut socket, _)) => {
                socket.set_nonblocking(false)?;
                socket.set_write_timeout(Some(Duration::from_millis(500)))?;
                for n in 0..150 {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if write!(socket, "SCB_DIAGNOSTIC_TICK_{n}\r\n").is_err() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
fn run_case(exe: &Path, root: &Path, discovery: &Discovery, logon: bool) -> Result<Value> {
    std::fs::create_dir_all(root.join("config/Sessions"))?;
    std::fs::write(root.join("config/Global.ini"),"D:\"Single Instance\"=00000000\r\nD:\"Show Connect Dialog\"=00000000\r\nD:\"Use Auto Session\"=00000000\r\n")?;
    scb_core::save_json(&root.join("connection.json"), discovery)?;
    let script = root.join("probe.scb");
    std::fs::write(
        &script,
        format!(
            "#$language = \"{PROG}\"\r\n#$interface = \"1.0\"\r\n{}\r\n",
            json!({"mode":"probe","test":if logon{"logon"}else{"startup"},"report_dir":root})
        ),
    )?;
    let display_stop = Arc::new(AtomicBool::new(false));
    let mut display_worker = None;
    let extra = if logon {
        std::fs::write(
            root.join("config/Sessions/Default.ini"),
            format!(
                "D:\"Use Script File\"=00000001\r\nS:\"Script Filename V2\"={}\r\n",
                script.display()
            ),
        )?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        let stop = display_stop.clone();
        display_worker = Some(std::thread::spawn(move || display_server(listener, stop)));
        vec!["/TELNET".into(), "127.0.0.1".into(), port.to_string()]
    } else {
        std::fs::write(
            root.join("config/Sessions/Default.ini"),
            "D:\"Use Script File\"=00000000\r\n",
        )?;
        vec!["/SCRIPT".into(), script.to_string_lossy().to_string()]
    };
    let result = (|| -> Result<Value> {
        let child = spawn_isolated(exe, &root.join("config"), &extra)?;
        let start = Instant::now();
        let mut completed = false;
        while start.elapsed() < Duration::from_secs(15) {
            if root.join("probe-report.json").exists() {
                completed = true;
                break;
            }
            if unsafe { WaitForSingleObject(child.0, 50) } == WAIT_OBJECT_0 {
                break;
            }
        }
        // Only terminate the process created for this case; never existing SSH.
        if unsafe { WaitForSingleObject(child.0, 100) } != WAIT_OBJECT_0 {
            unsafe {
                TerminateProcess(child.0, 0)?;
                WaitForSingleObject(child.0, 1000);
            }
        }
        Ok(
            json!({"probe_completed":completed,"report":scb_core::load_json::<Value>(&root.join("probe-report.json")).ok()}),
        )
    })();
    display_stop.store(true, Ordering::Relaxed);
    if let Some(worker) = display_worker {
        let _ = worker.join();
    }
    let _ = std::fs::remove_file(root.join("connection.json"));
    result
}
fn run() -> Result<Value> {
    if ENGINE.is_empty() {
        bail!("Build release payload before running diagnostics");
    }
    let install = Installation::load().context("No installed SecureCRT location found")?;
    platform::validate_exe(&install.securecrt_exe)?;
    let base = std::env::current_exe()?
        .parent()
        .context("Missing output directory")?
        .join("diagnostic-results");
    let root = base.join(format!("{}-{}", scb_core::now_ms(), &scb_core::id()[..8]));
    std::fs::create_dir_all(&root)?;
    let dll = root.join("probe-engine.dll");
    std::fs::write(&dll, ENGINE)?;
    let registration = Registration::create(&dll)?;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let discovery = Discovery {
        version: scb_core::VERSION.into(),
        port: listener.local_addr()?.port(),
        token: scb_core::id(),
        epoch: scb_core::id(),
        pid: std::process::id(),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let stop_server = stop.clone();
    let server_cfg = discovery.clone();
    let server = std::thread::spawn(move || {
        scb_core::broker::serve(
            listener,
            server_cfg,
            Arc::new(Mutex::new(scb_core::broker::State::default())),
            stop_server,
        )
    });
    let run_result = (|| -> Result<Value> {
        let startup = run_case(
            &install.securecrt_exe,
            &root.join("startup"),
            &discovery,
            false,
        )?;
        let logon = run_case(
            &install.securecrt_exe,
            &root.join("logon"),
            &discovery,
            true,
        )?;
        Ok(
            json!({"ok":true,"directory":root,"checks_passed":startup["report"]["checks_passed"]==true&&logon["report"]["checks_passed"]==true,"startup":startup,"logon":logon}),
        )
    })();
    stop.store(true, Ordering::Relaxed);
    let _ = server.join();
    drop(registration);
    let summary = match &run_result {
        Ok(v) => v.clone(),
        Err(e) => json!({"ok":false,"directory":root,"error":format!("{e:#}")}),
    };
    scb_core::save_json(&root.join("summary.json"), &summary)?;
    run_result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quotes_windows_arguments() {
        assert_eq!(quote("C:\\test dir\\"), "\"C:\\test dir\\\\\"");
        assert_eq!(quote("a\"b"), "\"a\\\"b\"");
    }
}
