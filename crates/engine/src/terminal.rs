use crate::dispatch::{string, Dispatch};
use anyhow::{bail, Context, Result};
use scb_core::{http, now_ms, Discovery, Installation};
use serde_json::{json, Value};
use std::{
    os::windows::process::CommandExt,
    sync::atomic::{AtomicBool, Ordering},
};
use windows::{
    core::{w, Interface},
    Win32::System::{
        Com::IDispatch,
        Diagnostics::Debug::ActiveScript::{IActiveScriptSite, SCRIPTINFO_IUNKNOWN},
    },
};

// Logon scripts enter with Synchronous=true. A resident actor must let received
// data reach the user's window while idle. Restore display even on early errors.
struct IdleDisplay<'a>(&'a Dispatch);
impl<'a> IdleDisplay<'a> {
    fn begin(screen: &'a Dispatch) -> Result<Self> {
        screen.set("Synchronous", false)?;
        Ok(Self(screen))
    }
}
impl Drop for IdleDisplay<'_> {
    fn drop(&mut self) {
        let _ = self.0.set("Synchronous", false);
    }
}

pub fn diagnostic(message: &str) {
    use std::io::Write;
    let path = scb_core::data_dir().join("diagnostics.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 256 * 1024) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{} {}", now_ms(), message);
    }
}
fn ensure_server() -> Result<Discovery> {
    if scb_core::data_dir().join("stopped").exists() {
        bail!("Bridge stopped by user");
    }
    if let Ok(d) = Discovery::load() {
        if http::call(&d, "GET", "/health", &json!({})).is_ok_and(|v| v["ok"] == true) {
            return Ok(d);
        }
    }
    let install = Installation::load()?;
    std::process::Command::new(install.install_dir.join("securecrt-bridge.exe"))
        .arg("serve")
        .creation_flags(0x08000000)
        .spawn()?;
    // The caller pumps SecureCRT between subsequent attempts; no prolonged wait here.
    bail!("Broker starting")
}
pub fn run(site: &IActiveScriptSite, stop: &AtomicBool) -> Result<()> {
    // A new SecureCRT logon starts the bridge again. Existing actors respect the
    // stop marker so choosing Exit does not immediately respawn the service.
    let _ = std::fs::remove_file(scb_core::data_dir().join("stopped"));
    let mut unk = None;
    unsafe {
        site.GetItemInfo(
            w!("crt"),
            SCRIPTINFO_IUNKNOWN,
            &mut unk,
            std::ptr::null_mut(),
        )?;
    }
    let crt = Dispatch(
        unk.context("SecureCRT did not expose crt")?
            .cast::<IDispatch>()?,
    );
    let tab = crt.object_call("GetScriptTab", vec![])?;
    let screen = tab.obj("Screen")?;
    let session = tab.obj("Session")?;
    let _display = IdleDisplay::begin(&screen)?;
    let mut sid = format!("{}-{}", std::process::id(), scb_core::id());
    let mut was_connected = false;
    let mut cfg = None;
    let mut next_start = 0;
    let mut last_transport_error = String::new();
    let mut announced = false;
    diagnostic("Native terminal attached");
    while !stop.load(Ordering::Relaxed) {
        // Retain this tab object, never re-resolve the selected tab or its index.
        let connected = session.boolean("Connected")?;
        if was_connected && !connected {
            if let Some(d) = cfg.as_ref() {
                let _ = http::call(
                    d,
                    "POST",
                    "/actor/poll",
                    &json!({"id":sid,"connected":false,"title":tab.text("Caption").unwrap_or_default()}),
                );
            }
            break;
        }
        if connected && !was_connected {
            sid = format!("{}-{}", std::process::id(), scb_core::id());
        }
        was_connected = connected;
        if cfg.is_none() && now_ms() >= next_start {
            match ensure_server() {
                Ok(d) => {
                    cfg = Some(d);
                    last_transport_error.clear();
                }
                Err(e) => {
                    let message = format!("Broker discovery: {e:#}");
                    if message != last_transport_error {
                        diagnostic(&message);
                        last_transport_error = message;
                    }
                }
            }
            next_start = now_ms() + 3000;
        }
        if let Some(d) = cfg.as_ref() {
            let rows = screen.int("Rows")?.clamp(1, 1000);
            let cols = screen.int("Columns")?.clamp(1, 2000);
            let snapshot = string(&screen.call(
                "Get",
                vec![1i32.into(), 1i32.into(), rows.into(), cols.into()],
            )?)?;
            match http::call(
                d,
                "POST",
                "/actor/poll",
                &json!({"id":sid,"title":tab.text("Caption")?,"connected":connected,"screen":snapshot}),
            ) {
                Ok(reply) if reply["ok"] == true => {
                    if !announced {
                        diagnostic("Native terminal registered; idle display asynchronous");
                        announced = true;
                    }
                    if reply["job"].is_object() {
                        let job = &reply["job"];
                        let result = match execute(&crt, &screen, job, stop) {
                            Ok(v) => v,
                            Err(_) => {
                                json!({"ok":false,"outcome":"unknown","error":"Terminal operation failed; inspect screen before taking further action"})
                            }
                        };
                        // Retrying the RESULT is safe. Never retry delivery or screen.Send.
                        let report = json!({"id":job["id"],"session":sid,"result":result});
                        for _ in 0..3 {
                            if http::call(d, "POST", "/actor/result", &report)
                                .is_ok_and(|v| v["ok"] == true)
                            {
                                break;
                            }
                            crt.call("Sleep", vec![150i32.into()])?;
                        }
                    }
                }
                Err(e) => {
                    let message = format!("Actor transport: {e:#}");
                    if message != last_transport_error {
                        diagnostic(&message);
                        last_transport_error = message;
                    }
                    cfg = None;
                    announced = false;
                }
                _ => {
                    diagnostic("Actor registration rejected by local broker");
                    cfg = None;
                    announced = false;
                }
            }
        }
        crt.call("Sleep", vec![200i32.into()])?;
    }
    // Restore normal asynchronous display if the user cancelled during a command.
    let _ = screen.set("Synchronous", false);
    diagnostic("Native terminal detached");
    Ok(())
}
fn execute(_crt: &Dispatch, screen: &Dispatch, job: &Value, stop: &AtomicBool) -> Result<Value> {
    let deadline = job["deadline"].as_u64().context("Missing deadline")?;
    if now_ms() >= deadline {
        return Ok(json!({"ok":false,"outcome":"not_sent","error":"Expired before dispatch"}));
    }
    let p = &job["payload"];
    if let Some(text) = p["text"].as_str() {
        screen.call("Send", vec![text.into()])?;
        return Ok(json!({"ok":true,"sent":true}));
    }
    let command = p["command"].as_str().context("Missing command")?;
    let id = job["id"].as_str().context("Missing id")?;
    let (wire, begin, end) = scb_core::frame_command(command, id);
    let old_sync = screen.boolean("Synchronous")?;
    let old_ignore = screen.boolean("IgnoreEscape").unwrap_or(false);
    screen.set("Synchronous", true)?;
    let result = (|| {
        screen.set("IgnoreEscape", true)?;
        screen.call("Send", vec![wire.as_str().into()])?;
        let before = read_until(screen, &begin, deadline, stop)?;
        if !before.1 {
            return Ok(
                json!({"ok":false,"outcome":"unknown","error":"Start marker timeout; command may still be running"}),
            );
        }
        let (output, matched) = read_until(screen, &format!("{end}:"), deadline, stop)?;
        if !matched {
            return Ok(
                json!({"ok":false,"outcome":"unknown","error":"Completion timeout; no automatic interrupt or retry","output":output}),
            );
        }
        let (code, finished) = read_until(screen, "\n", deadline, stop)?;
        Ok(
            json!({"ok":finished,"output":output.trim_start_matches(['\r','\n']),"exit_code":code.trim().parse::<i32>().ok(),"outcome":if finished{"completed"}else{"unknown"}}),
        )
    })();
    let _ = screen.set("IgnoreEscape", old_ignore);
    let _ = screen.set("Synchronous", old_sync);
    result
}
fn read_until(
    screen: &Dispatch,
    marker: &str,
    deadline: u64,
    stop: &AtomicBool,
) -> Result<(String, bool)> {
    let remaining = deadline.saturating_sub(now_ms());
    if remaining == 0 || stop.load(Ordering::Relaxed) {
        return Ok((String::new(), false));
    }
    // SecureCRT ReadString discards its partial capture and returns an empty
    // string on timeout. Retrying in one-second slices loses earlier output.
    // One call spans the remaining command deadline, as in the proven prototype.
    let seconds = remaining.div_ceil(1000).clamp(1, 300) as i32;
    let output = string(&screen.call("ReadString", vec![marker.into(), seconds.into()])?)?;
    if output.len() > 1024 * 1024 {
        bail!("Output limit exceeded; result unknown");
    }
    Ok((output, screen.int("MatchIndex")? != 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};
    use windows::{
        core::*,
        Win32::{
            Foundation::E_NOTIMPL,
            System::{Com::*, Variant::VARIANT},
        },
    };
    #[derive(Default)]
    struct Data {
        sent: Vec<String>,
        sync: bool,
        ignore: bool,
        reads: usize,
        delayed: bool,
        matched: bool,
        force_timeout: bool,
    }
    #[implement(IDispatch)]
    struct Mock {
        data: Rc<RefCell<Data>>,
    }
    const NAMES: [&str; 7] = [
        "Send",
        "Synchronous",
        "IgnoreEscape",
        "ReadString",
        "MatchIndex",
        "Sleep",
        "Get",
    ];
    impl IDispatch_Impl for Mock_Impl {
        fn GetTypeInfoCount(&self) -> windows::core::Result<u32> {
            Ok(0)
        }
        fn GetTypeInfo(&self, _: u32, _: u32) -> windows::core::Result<ITypeInfo> {
            Err(E_NOTIMPL.into())
        }
        fn GetIDsOfNames(
            &self,
            _: *const GUID,
            names: *const PCWSTR,
            count: u32,
            _: u32,
            ids: *mut i32,
        ) -> windows::core::Result<()> {
            unsafe {
                assert_eq!(count, 1);
                let name = (*names).to_string()?;
                *ids = NAMES.iter().position(|s| *s == name).unwrap() as i32;
            }
            Ok(())
        }
        fn Invoke(
            &self,
            id: i32,
            _: *const GUID,
            _: u32,
            flags: DISPATCH_FLAGS,
            p: *const DISPPARAMS,
            result: *mut VARIANT,
            _: *mut EXCEPINFO,
            _: *mut u32,
        ) -> windows::core::Result<()> {
            unsafe {
                let p = &*p;
                let args = if p.cArgs == 0 {
                    &[]
                } else {
                    std::slice::from_raw_parts(p.rgvarg, p.cArgs as usize)
                };
                let mut data = self.data.borrow_mut();
                let value = match NAMES[id as usize] {
                    "Send" => {
                        data.sent.push(string(&args[0]).unwrap());
                        VARIANT::default()
                    }
                    "Synchronous" | "IgnoreEscape" => {
                        let flag = if id == 1 {
                            &mut data.sync
                        } else {
                            &mut data.ignore
                        };
                        if flags == DISPATCH_PROPERTYPUT {
                            *flag = bool::try_from(&args[0])?;
                            VARIANT::default()
                        } else {
                            (*flag).into()
                        }
                    }
                    "ReadString" => {
                        let seconds = i32::try_from(&args[0])?;
                        assert!((1..=300).contains(&seconds));
                        let marker = string(&args[1]).unwrap();
                        data.reads += 1;
                        data.matched = true;
                        match data.reads {
                            1 => {
                                assert!(marker.ends_with("_BEGIN__"));
                                "echo\r\n".into()
                            }
                            2 => {
                                assert!(marker.ends_with("_END__:"));
                                if data.force_timeout || (data.delayed && seconds < 4) {
                                    data.matched = false;
                                    "".into()
                                } else if data.delayed {
                                    "\r\nBEFORE_DELAY\r\nAFTER_DELAY\r\n".into()
                                } else {
                                    "\r\n中文\r\nstderr\r\n".into()
                                }
                            }
                            3 => {
                                assert_eq!(marker, "\n");
                                "7\r".into()
                            }
                            _ => panic!("Capture must not retry after a destructive timeout"),
                        }
                    }
                    "MatchIndex" => i32::from(data.matched).into(),
                    _ => VARIANT::default(),
                };
                if !result.is_null() {
                    std::ptr::write(result, value);
                }
                Ok(())
            }
        }
    }
    #[test]
    fn native_dispatch_captures_unicode_exit_code_and_restores_screen() {
        let data = Rc::new(RefCell::new(Data::default()));
        let raw: IDispatch = Mock { data: data.clone() }.into();
        let screen = Dispatch(raw);
        let job = json!({"id":"test1","deadline":now_ms()+10000,"payload":{"command":"printf '中文'; exit 7"}});
        let result = execute(&screen, &screen, &job, &AtomicBool::new(false)).unwrap();
        assert_eq!(result["exit_code"], 7);
        assert_eq!(result["output"], "中文\r\nstderr\r\n");
        assert_eq!(data.borrow().sent.len(), 1);
        assert!(!data.borrow().sync);
        assert!(!data.borrow().ignore);
        assert!(!data.borrow().sent[0].contains("__SCB_test1_BEGIN__"));
    }
    #[test]
    fn expired_delivery_never_calls_send() {
        let data = Rc::new(RefCell::new(Data::default()));
        let raw: IDispatch = Mock { data: data.clone() }.into();
        let screen = Dispatch(raw);
        let job = json!({"id":"expired","deadline":now_ms()-1,"payload":{"text":"dangerous"}});
        assert_eq!(
            execute(&screen, &screen, &job, &AtomicBool::new(false)).unwrap()["outcome"],
            "not_sent"
        );
        assert!(data.borrow().sent.is_empty());
    }
    #[test]
    fn delayed_output_keeps_the_prefix_and_timeout_never_retries() {
        for force_timeout in [false, true] {
            let data = Rc::new(RefCell::new(Data {
                delayed: true,
                force_timeout,
                ..Default::default()
            }));
            let raw: IDispatch = Mock { data: data.clone() }.into();
            let screen = Dispatch(raw);
            let result=execute(&screen,&screen,&json!({"id":"delay","deadline":now_ms()+10000,"payload":{"command":"printf BEFORE_DELAY; sleep 4; printf AFTER_DELAY"}}),&AtomicBool::new(false)).unwrap();
            if force_timeout {
                assert_eq!(result["outcome"], "unknown");
                assert_eq!(data.borrow().reads, 2);
            } else {
                assert_eq!(result["output"], "BEFORE_DELAY\r\nAFTER_DELAY\r\n");
                assert_eq!(result["outcome"], "completed");
            }
            assert_eq!(data.borrow().sent.len(), 1);
            assert!(!data.borrow().sync);
        }
    }
    #[test]
    fn logon_buffer_is_disabled_and_error_cleanup_restores_display() {
        let data = Rc::new(RefCell::new(Data {
            sync: true,
            ..Default::default()
        }));
        let raw: IDispatch = Mock { data: data.clone() }.into();
        let screen = Dispatch(raw);
        let operation = (|| -> anyhow::Result<()> {
            let _display = IdleDisplay::begin(&screen)?;
            assert!(!data.borrow().sync);
            screen.set("Synchronous", true)?;
            bail!("Simulated host failure during command capture");
        })();
        assert!(operation.is_err());
        assert!(!data.borrow().sync);
        assert!(data.borrow().sent.is_empty());
    }
}
