//! Bounded read-only diagnostics. No Send/Connect/Disconnect and no global state.
use crate::dispatch::{string, Dispatch};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::{io::Write, path::Path, time::Instant};
use windows::{
    core::{w, Interface},
    Win32::System::{
        Com::IDispatch,
        Diagnostics::Debug::ActiveScript::{IActiveScriptSite, SCRIPTINFO_IUNKNOWN},
    },
};

fn trace(root: &Path, stage: &str) -> Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("stages.log"))?;
    writeln!(f, "{} {}", scb_core::now_ms(), stage)?;
    f.flush()?;
    Ok(())
}
fn network_check(discovery: &scb_core::Discovery, worker: bool) -> Value {
    let began = Instant::now();
    let mut completed = 0;
    let outcome = (|| -> Result<()> {
        for n in 0..20 {
            let reply = if n % 2 == 0 {
                scb_core::http::call(discovery, "GET", "/health", &json!({}))?
            } else {
                scb_core::http::call(
                    discovery,
                    "POST",
                    "/actor/poll",
                    &json!({
                        "id":if worker{"diagnostic-worker"}else{"diagnostic-host"},
                        "connected":false,"title":"isolated probe","screen":"sample 中文\r\n".repeat(200)
                    }),
                )?
            };
            anyhow::ensure!(reply["ok"] == true, "Broker rejected diagnostic request");
            completed += 1;
        }
        Ok(())
    })();
    json!({"ok":outcome.is_ok(),"completed":completed,"elapsed_ms":began.elapsed().as_millis()as u64,"error":outcome.err().map(|e|format!("{e:#}"))})
}
pub fn run(site: &IActiveScriptSite, meta: &Value) -> Result<()> {
    let root = Path::new(
        meta["report_dir"]
            .as_str()
            .context("Missing probe report directory")?,
    );
    trace(root, "entered")?;
    let mut result = json!({"ok":false,"remote_commands_sent":0});
    let outcome = (|| -> Result<()> {
        trace(root, "get_crt")?;
        let mut unk = None;
        unsafe {
            site.GetItemInfo(
                w!("crt"),
                SCRIPTINFO_IUNKNOWN,
                &mut unk,
                std::ptr::null_mut(),
            )?;
        }
        let crt = Dispatch(unk.context("Missing crt")?.cast::<IDispatch>()?);
        trace(root, "get_tab")?;
        let tab = crt.object_call("GetScriptTab", vec![])?;
        trace(root, "get_screen")?;
        let screen = tab.obj("Screen")?;
        trace(root, "get_session")?;
        let session = tab.obj("Session")?;
        trace(root, "connected_property")?;
        result["connected"] = session.boolean("Connected")?.into();
        trace(root, "synchronous_property")?;
        result["original_synchronous"] = screen.boolean("Synchronous")?.into();
        trace(root, "screen_dimensions")?;
        let rows = screen.int("Rows")?;
        let cols = screen.int("Columns")?;
        result["rows"] = rows.into();
        result["columns"] = cols.into();
        trace(root, "screen_get")?;
        result["screen_char_count"] = string(&screen.call(
            "Get",
            vec![1i32.into(), 1i32.into(), rows.into(), cols.into()],
        )?)?
        .chars()
        .count()
        .into();
        trace(root, "discovery_read")?;
        let discovery: scb_core::Discovery = scb_core::load_json(&root.join("connection.json"))?;
        trace(root, "health_on_host_thread")?;
        let began = Instant::now();
        result["host_thread_health"] =
            match scb_core::http::call(&discovery, "GET", "/health", &json!({})) {
                Ok(v) => v,
                Err(e) => json!({"ok":false,"error":format!("{e:#}")}),
            };
        result["host_thread_health_ms"] = (began.elapsed().as_millis() as u64).into();
        trace(root, "crt_sleep_100")?;
        let began = Instant::now();
        crt.call("Sleep", vec![100i32.into()])?;
        result["sleep_ms"] = (began.elapsed().as_millis() as u64).into();
        trace(root, "host_repeated_requests")?;
        result["host_requests"] = network_check(&discovery, false);
        trace(root, "worker_health")?;
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let _ = tx.send(network_check(&discovery, true));
        });
        let began = Instant::now();
        loop {
            match rx.try_recv() {
                Ok(v) => {
                    result["worker_requests"] = v;
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) if began.elapsed().as_secs() < 4 => {
                    crt.call("Sleep", vec![50i32.into()])?;
                }
                _ => {
                    result["worker_requests"] = json!({"ok":false,"error":"Worker timed out"});
                    break;
                }
            }
        }
        if meta["test"] == "logon" {
            trace(root, "logon_display_original")?;
            let mut previous = String::new();
            let mut original_updates = 0;
            let mut async_updates = 0;
            for n in 0..20 {
                if n == 10 {
                    trace(root, "logon_display_async")?;
                    screen.set("Synchronous", false)?;
                }
                crt.call("Sleep", vec![100i32.into()])?;
                let snapshot = string(&screen.call(
                    "Get",
                    vec![1i32.into(), 1i32.into(), rows.into(), cols.into()],
                )?)?;
                if snapshot.contains("SCB_DIAGNOSTIC_TICK_") && snapshot != previous {
                    if n < 10 {
                        original_updates += 1;
                    } else {
                        async_updates += 1;
                    }
                }
                previous = snapshot;
            }
            screen.set(
                "Synchronous",
                result["original_synchronous"].as_bool().unwrap_or(false),
            )?;
            result["original_display_updates"] = original_updates.into();
            result["async_display_updates"] = async_updates.into();
            result["connected_after"] = session.boolean("Connected")?.into();
        }
        result["checks_passed"] = (result["host_thread_health"]["ok"] == true
            && result["host_requests"]["ok"] == true
            && result["worker_requests"]["ok"] == true
            && (meta["test"] != "logon"
                || (result["connected_after"] == true
                    && result["async_display_updates"].as_u64().unwrap_or(0) > 0)))
            .into();
        result["ok"] = true.into();
        Ok(())
    })();
    if let Err(e) = outcome {
        result["error"] = format!("{e:#}").into();
    }
    trace(root, "writing_report")?;
    scb_core::save_json(&root.join("probe-report.json"), &result)?;
    trace(root, "completed")?;
    Ok(())
}
