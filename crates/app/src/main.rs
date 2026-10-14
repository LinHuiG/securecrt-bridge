#![windows_subsystem = "windows"]
mod installer;
mod installer_ui;
mod platform;
mod skills;
mod tray;
use anyhow::{bail, Context, Result};
use scb_core::{http, Discovery, Installation};
use serde_json::{json, Value};
use std::{
    net::{Ipv4Addr, TcpListener},
    os::windows::process::CommandExt,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
include!(concat!(env!("OUT_DIR"), "/payload.rs"));
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let gui = args.is_empty() || args.iter().any(|s| s == "--gui");
    if let Err(e) = run(&args) {
        let value = json!({"ok":false,"error":format!("{e:#}")});
        eprintln!("{}", value);
        if gui {
            platform::message(&format!("{e:#}"), true);
        }
        std::process::exit(1);
    }
}
fn option<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}
fn output(value: Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    if value["ok"] == false {
        bail!("Local API reported failure");
    }
    Ok(())
}
fn local(method: &str, path: &str, v: &Value) -> Result<Value> {
    http::call(
        &Discovery::load().context("Bridge is not running. Launch securecrt-bridge.exe start")?,
        method,
        path,
        v,
    )
}
fn run(args: &[String]) -> Result<()> {
    let command = args.first().map(String::as_str).unwrap_or("install");
    if args.is_empty()
        || (matches!(command, "install" | "repair" | "uninstall")
            && args.iter().any(|a| a == "--gui"))
    {
        return installer_ui::run(args);
    }
    match command {
        "setup" => installer_ui::run(args),
        "--installer-self-test" => output(installer_ui::self_test()?),
        "install" | "repair" => output(installer::install(args, command == "repair")?),
        "uninstall" => output(installer::uninstall(args)?),
        "detect" => output(
            json!({"ok":true,"candidates":installer::detect(),"skill_clients":skills::discover(Installation::load().ok().as_ref())}),
        ),
        "serve" => serve(!args.iter().any(|a| a == "--no-tray")),
        "start" => {
            start()?;
            output(json!({"ok":true,"started":true}))
        }
        "stop" => {
            std::fs::write(scb_core::data_dir().join("stopped"), b"user")?;
            output(local("POST", "/shutdown", &json!({}))?)
        }
        "status" | "health" => output(local("GET", "/health", &json!({}))?),
        "sessions" => output(local("GET", "/sessions", &json!({}))?),
        "doctor" => {
            let install = Installation::load().ok();
            let connection = Discovery::load().ok();
            let health = connection
                .as_ref()
                .and_then(|d| http::call(d, "GET", "/health", &json!({})).ok());
            output(
                json!({"ok":true,"version":scb_core::VERSION,"installation":install,"data_directory":scb_core::data_dir(),"port":connection.map(|d|d.port),"health":health,"securecrt_processes":platform::securecrt_processes().ok(),"engine_registered":platform::reg_get(windows::Win32::System::Registry::HKEY_CURRENT_USER,&format!("Software\\Classes\\CLSID\\{}\\InprocServer32",scb_core::ENGINE_CLSID),"")}),
            )
        }
        "pause" | "resume" => {
            let path = if let Some(id) = args.get(1).filter(|v| !v.starts_with('-')) {
                format!("/sessions/{id}/pause")
            } else {
                "/pause".into()
            };
            output(local("POST", &path, &json!({"paused":command=="pause"}))?)
        }
        "result" => output(local(
            "GET",
            &format!("/jobs/{}", args.get(1).context("result REQUEST_ID")?),
            &json!({}),
        )?),
        "screen" => output(local(
            "GET",
            &format!(
                "/sessions/{}/screen",
                args.get(1).context("screen SESSION_ID")?
            ),
            &json!({}),
        )?),
        "claim" | "release" | "acknowledge" => {
            let id = args.get(1).context("Provide SESSION_ID")?;
            let owner = option(args, "--owner").context("Provide --owner CHAT_ID")?;
            output(local(
                "POST",
                &format!("/sessions/{id}/{command}"),
                &json!({"owner":owner}),
            )?)
        }
        "exec" | "input" | "interrupt" => {
            let id = args.get(1).context("Provide SESSION_ID")?;
            let owner = option(args, "--owner").context("Provide --owner CHAT_ID")?;
            let text = if command == "interrupt" {
                option(args, "--job")
                    .context("Provide --job ORIGINAL_REQUEST_ID")?
                    .to_owned()
            } else if let Some(file) = option(args, "--file") {
                std::fs::read_to_string(file)?
            } else if args.iter().any(|a| a == "--stdin") {
                use std::io::Read;
                let mut text = String::new();
                std::io::stdin().take(32769).read_to_string(&mut text)?;
                text
            } else {
                option(
                    args,
                    if command == "exec" {
                        "--command"
                    } else {
                        "--text"
                    },
                )
                .context("Provide --command TEXT / --text TEXT, --file PATH, or --stdin")?
                .to_owned()
            };
            let timeout = option(args, "--timeout")
                .unwrap_or("30")
                .parse::<u64>()?
                .clamp(1, 300);
            let request_id = option(args, "--request-id")
                .map(String::from)
                .unwrap_or_else(scb_core::id);
            // Print the id before dispatch, so even a failed HTTP response is recoverable.
            eprintln!("request_id={request_id}");
            let mut p = json!({"owner":owner,"request_id":request_id,"timeout_seconds":timeout});
            p[match command {
                "exec" => "command",
                "interrupt" => "job_id",
                _ => "text",
            }] = text.into();
            let result = local("POST", &format!("/sessions/{id}/{command}"), &p)?;
            if result["ok"] != true {
                return output(result);
            }
            if args.iter().any(|s| s == "--async") {
                return output(result);
            }
            let end = Instant::now() + Duration::from_secs(timeout + 5);
            loop {
                let result = local("GET", &format!("/jobs/{request_id}"), &json!({}))?;
                if !matches!(result["status"].as_str(), Some("queued" | "running")) {
                    return output(result);
                }
                if Instant::now() >= end {
                    return output(
                        json!({"ok":false,"request_id":request_id,"error":"Result unresolved. Query result with this id; do not resend."}),
                    );
                }
                std::thread::sleep(Duration::from_millis(150));
            }
        }
        "help" | "--help" | "-h" => {
            println!("interrupt SESSION --owner CHAT --job ORIGINAL_REQUEST_ID [--request-id ID] [--async]\nSends Ctrl+C for your unknown job; inspect the fresh screen, then acknowledge.");
            println!("SecureCRT Bridge {}\ninstall [--path SecureCRT.exe] [--config Config] [--skills auto|none] [--skill-dir ABSOLUTE_DIR ...] | repair | uninstall\ndetect | start | stop | doctor | sessions | screen SESSION\nclaim SESSION --owner CHAT | release SESSION --owner CHAT\nexec SESSION --owner CHAT --command TEXT [--timeout 30] [--request-id ID] [--async]\ninput SESSION --owner CHAT --text TEXT | result REQUEST_ID\npause [SESSION] | resume [SESSION]\nCommand text may also be provided by --file UTF8_FILE or --stdin.",scb_core::VERSION);
            Ok(())
        }
        _ => bail!("Unknown command; use --help"),
    }
}
fn start() -> Result<()> {
    let _ = std::fs::remove_file(scb_core::data_dir().join("stopped"));
    if local("GET", "/health", &json!({})).is_ok_and(|v| v["ok"] == true) {
        return Ok(());
    }
    let exe = Installation::load()
        .map(|i| i.install_dir.join("securecrt-bridge.exe"))
        .unwrap_or(std::env::current_exe()?);
    std::process::Command::new(exe)
        .arg("serve")
        .creation_flags(0x08000000)
        .spawn()?;
    for _ in 0..30 {
        if local("GET", "/health", &json!({})).is_ok_and(|v| v["ok"] == true) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    bail!("Service did not become reachable; run doctor")
}
fn serve(show_tray: bool) -> Result<()> {
    let Some(_mutex) = platform::singleton()? else {
        return Ok(());
    };
    platform::private_directory(&scb_core::data_dir())?;
    let _ = std::fs::remove_file(scb_core::data_dir().join("stopped"));
    let listener = (18765..18800)
        .find_map(|port| TcpListener::bind((Ipv4Addr::LOCALHOST, port)).ok())
        .context("No available loopback port")?;
    let cfg = Discovery {
        version: scb_core::VERSION.into(),
        port: listener.local_addr()?.port(),
        token: format!("{}{}", scb_core::id(), scb_core::id()),
        epoch: scb_core::id(),
        pid: std::process::id(),
    };
    scb_core::save_json(&scb_core::data_dir().join("connection.json"), &cfg)?;
    let mut state = scb_core::broker::State::default();
    state.persist_preferences_at(scb_core::data_dir().join("preferences.json"));
    if let Ok(p) = scb_core::load_json::<Value>(&scb_core::data_dir().join("preferences.json")) {
        state.paused = p["paused"] == true;
    }
    let shared = Arc::new(Mutex::new(state));
    let stop = Arc::new(AtomicBool::new(false));
    let server_state = shared.clone();
    let server_stop = stop.clone();
    let saved = cfg.clone();
    let server = std::thread::spawn(move || {
        let r = scb_core::broker::serve(listener, cfg, server_state, server_stop.clone());
        server_stop.store(true, Ordering::Relaxed);
        r
    });
    if show_tray {
        tray::run(shared.clone(), stop.clone())?;
        stop.store(true, Ordering::Relaxed);
    }
    let result = server
        .join()
        .map_err(|_| anyhow::anyhow!("Server thread failed"))?;
    scb_core::save_json(
        &scb_core::data_dir().join("preferences.json"),
        &json!({"paused":shared.lock().unwrap_or_else(|e|e.into_inner()).paused}),
    )?;
    if Discovery::load().is_ok_and(|d| d.epoch == saved.epoch) {
        let _ = std::fs::remove_file(scb_core::data_dir().join("connection.json"));
    }
    result
}
