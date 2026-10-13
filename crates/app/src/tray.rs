use anyhow::Result;
use scb_core::{broker::Shared, w};
use serde_json::json;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, OnceLock,
};
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::*,
        System::LibraryLoader::GetModuleHandleW,
        UI::{Shell::*, WindowsAndMessaging::*},
    },
};
struct Context {
    state: Shared,
    stop: Arc<AtomicBool>,
}
static CTX: OnceLock<Context> = OnceLock::new();
const CALLBACK: u32 = WM_APP + 21;
pub fn run(state: Shared, stop: Arc<AtomicBool>) -> Result<()> {
    unsafe {
        let _ = CTX.set(Context { state, stop });
        let instance = GetModuleHandleW(None)?;
        let name = w("SecureCRTBridgeTray");
        let wc = WNDCLASSW {
            hInstance: instance.into(),
            lpszClassName: PCWSTR(name.as_ptr()),
            lpfnWndProc: Some(proc),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(name.as_ptr()),
            PCWSTR(name.as_ptr()),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        let mut icon = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: CALLBACK,
            hIcon: LoadIconW(None, IDI_APPLICATION)?,
            ..Default::default()
        };
        let tip = w("SecureCRT Bridge — 右键暂停或退出");
        icon.szTip[..tip.len()].copy_from_slice(&tip);
        let _ = Shell_NotifyIconW(NIM_ADD, &icon);
        SetTimer(Some(hwnd), 1, 500, None);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = Shell_NotifyIconW(NIM_DELETE, &icon);
        let _ = DestroyWindow(hwnd);
        Ok(())
    }
}
unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(ctx) = CTX.get() else {
        return DefWindowProcW(hwnd, msg, wp, lp);
    };
    if msg == WM_TIMER && ctx.stop.load(Ordering::Relaxed) {
        PostQuitMessage(0);
        return LRESULT(0);
    }
    if msg == CALLBACK && (lp.0 as u32 == WM_RBUTTONUP || lp.0 as u32 == WM_LBUTTONUP) {
        if let Ok(menu) = CreatePopupMenu() {
            let snapshot = ctx
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .request("GET", "/sessions", json!({}))
                .1;
            let paused = snapshot["paused"] == true;
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                1,
                PCWSTR(
                    w(if paused {
                        "恢复全部会话"
                    } else {
                        "暂停全部会话"
                    })
                    .as_ptr(),
                ),
            );
            let _ = AppendMenuW(menu, MF_STRING, 2, PCWSTR(w("状态").as_ptr()));
            let sessions = snapshot["sessions"].as_array().cloned().unwrap_or_default();
            if !sessions.is_empty() {
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            }
            for (i, s) in sessions.iter().take(100).enumerate() {
                let title = format!(
                    "{} {}",
                    if s["paused"] == true {
                        "恢复"
                    } else {
                        "暂停"
                    },
                    s["title"].as_str().unwrap_or("Session")
                );
                let _ = AppendMenuW(menu, MF_STRING, 100 + i, PCWSTR(w(title).as_ptr()));
            }
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, 3, PCWSTR(w("退出服务").as_ptr()));
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = SetForegroundWindow(hwnd);
            let selected = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_NONOTIFY,
                pt.x,
                pt.y,
                Some(0),
                hwnd,
                None,
            )
            .0 as usize;
            let _ = DestroyMenu(menu);
            match selected{
                1=>{let result=ctx.state.lock().unwrap_or_else(|e|e.into_inner()).request("POST","/pause",json!({"paused":!paused})).1;if result["ok"]!=true{crate::platform::message("无法保存暂停设置，请检查运行目录的写入权限。",true);}}
                2=>crate::platform::message(&format!("SecureCRT Bridge {}\n{} 个已接入会话\n状态：{}\n\n本地接口只接受带令牌的本机请求。\n暂停会取消尚未发送的命令；已发送的命令请在终端按 Ctrl+C 中断。",scb_core::VERSION,sessions.len(),if paused{"暂停"}else{"运行"}),false),
                3=>{let _=std::fs::write(scb_core::data_dir().join("stopped"),b"user");ctx.stop.store(true,Ordering::Relaxed);PostQuitMessage(0);}
                n if n>=100&&n<100+sessions.len()=>{let s=&sessions[n-100];if let Some(id)=s["id"].as_str(){ctx.state.lock().unwrap_or_else(|e|e.into_inner()).request("POST",&format!("/sessions/{id}/pause"),json!({"paused":s["paused"]!=true}));}}
                _=>{}
            }
        }
        return LRESULT(0);
    }
    if msg == WM_DESTROY {
        PostQuitMessage(0);
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}
