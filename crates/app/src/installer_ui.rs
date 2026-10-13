//! Native setup window. UI owns only presentation; all mutations stay in installer.
use crate::installer::{self, Candidate};
use anyhow::{Context, Result};
use scb_core::{w, Installation};
use serde_json::{json, Value};
use std::{
    cell::RefCell,
    path::Path,
    sync::mpsc::{self, Receiver, Sender},
};
use windows::Win32::System::SystemServices::{SS_PATHELLIPSIS, SS_RIGHT};
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::{Com::*, LibraryLoader::GetModuleHandleW},
        UI::{
            Controls::*, Input::KeyboardAndMouse::EnableWindow, Shell::*, WindowsAndMessaging::*,
        },
    },
};
const INFER_PATH: u32 = WM_APP + 1;

const INSTALL: usize = 101;
const UNINSTALL: usize = 102;
const PATH_EDIT: usize = 103;
const PATH_BROWSE: usize = 104;
const CONFIG_EDIT: usize = 105;
const CONFIG_BROWSE: usize = 106;
const DETECT: usize = 107;
const START: usize = 108;
const CLOSE: usize = 109;
enum Event {
    Detected(Vec<Candidate>),
    Step(u32, String),
    Finished(std::result::Result<Value, String>),
}
struct Ui {
    window: HWND,
    install: HWND,
    uninstall: HWND,
    path: HWND,
    config: HWND,
    path_browse: HWND,
    config_browse: HWND,
    destination: HWND,
    status: HWND,
    percent: HWND,
    progress: HWND,
    log: HWND,
    detect: HWND,
    start: HWND,
    close: HWND,
    title_font: HFONT,
    font: HFONT,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    running: bool,
    detecting: bool,
    remove: bool,
    steps: usize,
    installed: Option<Installation>,
}
thread_local! {static UI:RefCell<Option<Ui>>=const{RefCell::new(None)};}
fn get_text(hwnd: HWND) -> String {
    unsafe {
        let mut text = vec![0; GetWindowTextLengthW(hwnd).max(0) as usize + 1];
        let n = GetWindowTextW(hwnd, &mut text);
        String::from_utf16_lossy(&text[..n.max(0) as usize])
    }
}
fn set_text(hwnd: HWND, text: &str) {
    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR(w(text).as_ptr()));
    }
}
fn send(hwnd: HWND, msg: u32, wp: usize, lp: isize) -> LRESULT {
    unsafe { SendMessageW(hwnd, msg, Some(WPARAM(wp)), Some(LPARAM(lp))) }
}
fn enabled(hwnd: HWND, on: bool) {
    unsafe {
        let _ = EnableWindow(hwnd, on);
    }
}
fn append(ui: &mut Ui, text: &str) {
    ui.steps += 1;
    let line = w(format!(
        "[{:02}] {}\r\n",
        ui.steps,
        text.replace('\n', "\r\n")
    ));
    send(ui.log, EM_SETSEL, usize::MAX, -1);
    send(ui.log, EM_REPLACESEL, 0, line.as_ptr() as isize);
    send(ui.log, EM_SCROLLCARET, 0, 0);
}
fn state_controls(ui: &Ui) {
    let idle = !ui.running && !ui.detecting;
    for hwnd in [ui.install, ui.detect] {
        enabled(hwnd, idle);
    }
    enabled(ui.uninstall, idle && ui.installed.is_some());
    for hwnd in [ui.path, ui.config, ui.path_browse, ui.config_browse] {
        enabled(hwnd, idle && !ui.remove);
    }
    enabled(ui.start, idle && (!ui.remove || ui.installed.is_some()));
    enabled(ui.close, !ui.running);
    set_text(
        ui.start,
        if ui.remove {
            "开始卸载"
        } else {
            "开始安装 / 升级"
        },
    );
}
fn set_candidate(ui: &Ui, c: &Candidate) {
    set_text(
        ui.path,
        &c.launcher.parent().unwrap_or(&c.launcher).to_string_lossy(),
    );
    set_text(ui.config, &c.config.to_string_lossy());
    set_text(
        ui.destination,
        &format!(
            "桥接组件目录：{}",
            c.launcher
                .parent()
                .unwrap_or(&c.launcher)
                .join("SecureCRTBridge")
                .display()
        ),
    );
}
fn mode(remove: bool) {
    UI.with(|cell| {
        if let Some(ui) = cell.borrow_mut().as_mut() {
            if ui.running || ui.detecting {
                return;
            }
            ui.remove = remove && ui.installed.is_some();
            send(ui.install, BM_SETCHECK, usize::from(!ui.remove), 0);
            send(ui.uninstall, BM_SETCHECK, usize::from(ui.remove), 0);
            if ui.remove {
                if let Some(i) = &ui.installed {
                    set_candidate(
                        ui,
                        &Candidate {
                            launcher: i.launcher.clone(),
                            executable: i.securecrt_exe.clone(),
                            config: i.config_dir.clone(),
                        },
                    );
                }
            }
            set_text(
                ui.status,
                if ui.remove {
                    "准备卸载：恢复原配置，保留备份及用户修改的文件"
                } else {
                    "请选择路径，然后开始安装。执行前请关闭 SecureCRT。"
                },
            );
            state_controls(ui);
        }
    });
}
fn detect() {
    UI.with(|cell| {
        if let Some(ui) = cell.borrow_mut().as_mut() {
            if ui.running || ui.detecting {
                return;
            }
            ui.detecting = true;
            set_text(ui.status, "正在查找 SecureCRT，请稍候…");
            append(ui, "开始查找本机 SecureCRT 安装位置");
            state_controls(ui);
            let tx = ui.tx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(Event::Detected(installer::detect()));
            });
        }
    });
}
fn infer_path() {
    let path = UI.with(|c| {
        c.borrow()
            .as_ref()
            .filter(|u| !u.running && !u.remove)
            .map(|u| get_text(u.path))
    });
    if let Some(path) = path {
        if let Some(candidate) = installer::candidate(Path::new(path.trim())) {
            UI.with(|c| {
                if let Some(ui) = c.borrow().as_ref() {
                    set_candidate(ui, &candidate);
                }
            });
        }
    }
}
fn choose_folder(owner: HWND, title: &str) -> Result<Option<String>> {
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dialog.SetOptions(
            dialog.GetOptions()? | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST,
        )?;
        dialog.SetTitle(PCWSTR(w(title).as_ptr()))?;
        if let Err(error) = dialog.Show(Some(owner)) {
            if error.code() == windows::core::HRESULT::from_win32(ERROR_CANCELLED.0) {
                return Ok(None);
            }
            return Err(error.into());
        }
        let raw = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = raw.to_string();
        CoTaskMemFree(Some(raw.0.cast()));
        Ok(Some(path?))
    }
}
fn browse(config: bool) {
    let window = UI.with(|c| c.borrow().as_ref().map(|ui| ui.window));
    if let Some(window) = window {
        match choose_folder(
            window,
            if config {
                "选择包含 Sessions 文件夹的配置目录"
            } else {
                "选择 SecureCRT 安装目录"
            },
        ) {
            Ok(Some(path)) => {
                UI.with(|c| {
                    if let Some(ui) = c.borrow().as_ref() {
                        set_text(if config { ui.config } else { ui.path }, &path);
                    }
                });
                if !config {
                    infer_path();
                }
            }
            Ok(None) => {}
            Err(error) => UI.with(|c| {
                if let Some(ui) = c.borrow_mut().as_mut() {
                    append(ui, &format!("选择路径失败：{error:#}"));
                }
            }),
        }
    }
}
fn start() {
    let task = UI.with(|c| {
        let mut state = c.borrow_mut();
        let ui = state.as_mut()?;
        if ui.running || ui.detecting {
            return None;
        }
        let path = get_text(ui.path).trim().to_owned();
        let config = get_text(ui.config).trim().to_owned();
        if !ui.remove && (path.is_empty() || config.is_empty()) {
            set_text(ui.status, "请先选择 SecureCRT 安装目录和配置目录。");
            return None;
        }
        ui.running = true;
        ui.steps = 0;
        set_text(ui.log, "");
        send(ui.progress, PBM_SETPOS, 0, 0);
        set_text(ui.percent, "0%");
        state_controls(ui);
        let args = if ui.remove {
            vec!["uninstall".into(), "--noninteractive".into()]
        } else {
            vec![
                "install".into(),
                "--path".into(),
                path,
                "--config".into(),
                config,
                "--noninteractive".into(),
            ]
        };
        Some((ui.tx.clone(), ui.remove, args))
    });
    if let Some((tx, remove, args)) = task {
        std::thread::spawn(move || {
            let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
            let progress = |percent, text: &str| {
                let _ = tx.send(Event::Step(percent, text.to_owned()));
            };
            let result = if remove {
                installer::uninstall_with_progress(&args, &progress)
            } else {
                installer::install_with_progress(&args, false, &progress)
            };
            if initialized {
                unsafe {
                    CoUninitialize();
                }
            }
            let _ = tx.send(Event::Finished(result.map_err(|e| format!("{e:#}"))));
        });
    }
}
fn drain() {
    UI.with(|cell|{let mut state=cell.borrow_mut();let Some(ui)=state.as_mut()else{return;};
    while let Ok(event)=ui.rx.try_recv(){match event{
        Event::Detected(found)=>{
            ui.detecting=false;
            if get_text(ui.path).trim().is_empty(){if let Some(c)=found.first(){set_candidate(ui,c);}}
            if found.is_empty(){append(ui,"未自动找到 SecureCRT，请点击“浏览…”手动选择。");}
            else{for c in &found{append(ui,&format!("发现：{}",c.launcher.display()));}}
            set_text(ui.status,if ui.remove{"准备卸载：恢复原配置，保留备份及用户修改的文件"}else{"路径已就绪。请确认后开始安装；执行前请关闭 SecureCRT。"});state_controls(ui);
        },
        Event::Step(percent,text)=>{send(ui.progress,PBM_SETPOS,percent.min(100)as usize,0);set_text(ui.percent,&format!("{}%",percent.min(100)));set_text(ui.status,&text);append(ui,&text);},
        Event::Finished(result)=>{
            ui.running=false;ui.installed=Installation::load().ok();
            match result{
                Ok(value)=>{
                    let retained=value["retained"].as_array().map_or(0,Vec::len);let skipped=value["skipped"].as_array().map_or(0,Vec::len);
                    let text=if ui.remove{format!("卸载完成。原配置备份已保留；保留项：{retained}。")}
                        else{format!("安装完成。现在可以从齐治连接 SSH；保留原配置的会话：{skipped}。")};
                    set_text(ui.status,&text);append(ui,&text);
                },
                Err(error)=>{set_text(ui.status,"未完成，请查看下方原因；解决后可重试。");append(ui,&format!("错误：{error}"));}
            }state_controls(ui);
        }
    }}
});
}
unsafe extern "system" fn proc(window: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = wp.0 & 0xffff;
            let notification = (wp.0 >> 16) & 0xffff;
            if notification == BN_CLICKED as usize {
                match id {
                    INSTALL => mode(false),
                    UNINSTALL => mode(true),
                    PATH_BROWSE => browse(false),
                    CONFIG_BROWSE => browse(true),
                    DETECT => detect(),
                    START => start(),
                    CLOSE => {
                        let _ = PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                    _ => {}
                }
            } else if id == PATH_EDIT && notification == EN_KILLFOCUS as usize {
                let _ = PostMessageW(Some(window), INFER_PATH, WPARAM(0), LPARAM(0));
            }
            return LRESULT(0);
        }
        INFER_PATH => {
            infer_path();
            return LRESULT(0);
        }
        WM_TIMER => {
            drain();
            return LRESULT(0);
        }
        WM_CLOSE => {
            let running = UI.with(|c| c.borrow().as_ref().is_some_and(|u| u.running));
            if !running {
                let _ = DestroyWindow(window);
            }
            return LRESULT(0);
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(window, msg, wp, lp)
}
unsafe fn control(
    parent: HWND,
    class: &str,
    text: &str,
    style: WINDOW_STYLE,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    id: usize,
    font: HFONT,
) -> Result<HWND> {
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        PCWSTR(w(class).as_ptr()),
        PCWSTR(w(text).as_ptr()),
        WS_CHILD | WS_VISIBLE | style,
        x,
        y,
        width,
        height,
        Some(parent),
        Some(HMENU(id as *mut _)),
        None,
        None,
    )?;
    send(hwnd, WM_SETFONT, font.0 as usize, 1);
    Ok(hwnd)
}
unsafe fn create(args: &[String]) -> Result<HWND> {
    anyhow::ensure!(
        InitCommonControlsEx(&INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_PROGRESS_CLASS
        })
        .as_bool(),
        "Cannot initialize progress controls"
    );
    let instance = GetModuleHandleW(None)?;
    let class = w("SecureCRTBridgeSetup");
    let wc = WNDCLASSW {
        hInstance: instance.into(),
        lpszClassName: PCWSTR(class.as_ptr()),
        lpfnWndProc: Some(proc),
        hCursor: LoadCursorW(None, IDC_ARROW)?,
        hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as *mut _),
        ..Default::default()
    };
    RegisterClassW(&wc);
    let style = WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
    let mut size = RECT {
        left: 0,
        top: 0,
        right: 760,
        bottom: 645,
    };
    AdjustWindowRectEx(&mut size, style, false, WINDOW_EX_STYLE::default())?;
    let window = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        PCWSTR(class.as_ptr()),
        PCWSTR(w(format!("SecureCRT Bridge {} 安装与卸载", scb_core::VERSION)).as_ptr()),
        style,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        size.right - size.left,
        size.bottom - size.top,
        None,
        None,
        Some(instance.into()),
        None,
    )?;
    let font = CreateFontW(
        -17,
        0,
        0,
        0,
        400,
        0,
        0,
        0,
        DEFAULT_CHARSET,
        OUT_DEFAULT_PRECIS,
        CLIP_DEFAULT_PRECIS,
        CLEARTYPE_QUALITY,
        DEFAULT_PITCH.0 as u32,
        PCWSTR(w("Microsoft YaHei UI").as_ptr()),
    );
    let title_font = CreateFontW(
        -27,
        0,
        0,
        0,
        600,
        0,
        0,
        0,
        DEFAULT_CHARSET,
        OUT_DEFAULT_PRECIS,
        CLIP_DEFAULT_PRECIS,
        CLEARTYPE_QUALITY,
        DEFAULT_PITCH.0 as u32,
        PCWSTR(w("Microsoft YaHei UI").as_ptr()),
    );
    control(
        window,
        "STATIC",
        "SecureCRT Bridge",
        WINDOW_STYLE(0),
        24,
        18,
        710,
        38,
        0,
        title_font,
    )?;
    control(
        window,
        "STATIC",
        "保留现有终端，为 SSH 会话增加本地共享接口",
        WINDOW_STYLE(0),
        26,
        59,
        700,
        26,
        0,
        font,
    )?;
    let install = control(
        window,
        "BUTTON",
        "安装 / 升级",
        WS_TABSTOP | WS_GROUP | WINDOW_STYLE(BS_AUTORADIOBUTTON as u32),
        24,
        95,
        180,
        28,
        INSTALL,
        font,
    )?;
    let uninstall = control(
        window,
        "BUTTON",
        "卸载桥接",
        WS_TABSTOP | WINDOW_STYLE(BS_AUTORADIOBUTTON as u32),
        225,
        95,
        170,
        28,
        UNINSTALL,
        font,
    )?;
    control(
        window,
        "STATIC",
        "SecureCRT 安装目录",
        WINDOW_STYLE(0),
        24,
        138,
        500,
        23,
        0,
        font,
    )?;
    let path = control(
        window,
        "EDIT",
        "",
        WS_TABSTOP | WS_BORDER | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
        24,
        165,
        598,
        30,
        PATH_EDIT,
        font,
    )?;
    let path_browse = control(
        window,
        "BUTTON",
        "浏览…",
        WS_TABSTOP,
        634,
        165,
        102,
        30,
        PATH_BROWSE,
        font,
    )?;
    control(
        window,
        "STATIC",
        "SecureCRT 配置目录（包含 Sessions 文件夹）",
        WINDOW_STYLE(0),
        24,
        207,
        690,
        23,
        0,
        font,
    )?;
    let config = control(
        window,
        "EDIT",
        "",
        WS_TABSTOP | WS_BORDER | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
        24,
        234,
        598,
        30,
        CONFIG_EDIT,
        font,
    )?;
    let config_browse = control(
        window,
        "BUTTON",
        "浏览…",
        WS_TABSTOP,
        634,
        234,
        102,
        30,
        CONFIG_BROWSE,
        font,
    )?;
    let destination = control(
        window,
        "STATIC",
        "桥接组件将安装到所选目录的 SecureCRTBridge 子目录。",
        WINDOW_STYLE(SS_PATHELLIPSIS.0),
        24,
        275,
        712,
        25,
        0,
        font,
    )?;
    control(
        window,
        "STATIC",
        "开始前请关闭 SecureCRT。卸载将恢复原配置，并保留备份。",
        WINDOW_STYLE(0),
        24,
        306,
        712,
        26,
        0,
        font,
    )?;
    let status = control(
        window,
        "STATIC",
        "请选择路径，然后开始安装。",
        WINDOW_STYLE(SS_PATHELLIPSIS.0),
        24,
        344,
        640,
        26,
        0,
        font,
    )?;
    let percent = control(
        window,
        "STATIC",
        "0%",
        WINDOW_STYLE(SS_RIGHT.0),
        670,
        344,
        66,
        26,
        0,
        font,
    )?;
    let progress = control(
        window,
        "msctls_progress32",
        "",
        WINDOW_STYLE(0),
        24,
        376,
        712,
        22,
        0,
        font,
    )?;
    send(progress, PBM_SETRANGE32, 0, 100);
    control(
        window,
        "STATIC",
        "执行步骤",
        WINDOW_STYLE(0),
        24,
        414,
        712,
        23,
        0,
        font,
    )?;
    let log = control(
        window,
        "EDIT",
        "",
        WS_TABSTOP
            | WS_BORDER
            | WS_VSCROLL
            | WINDOW_STYLE((ES_MULTILINE | ES_AUTOVSCROLL | ES_READONLY) as u32),
        24,
        442,
        712,
        143,
        0,
        font,
    )?;
    send(log, EM_SETLIMITTEXT, 1024 * 1024, 0);
    let detect = control(
        window,
        "BUTTON",
        "重新检测路径",
        WS_TABSTOP,
        24,
        602,
        150,
        32,
        DETECT,
        font,
    )?;
    let start = control(
        window,
        "BUTTON",
        "开始安装 / 升级",
        WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
        418,
        602,
        184,
        32,
        START,
        font,
    )?;
    let close = control(
        window, "BUTTON", "关闭", WS_TABSTOP, 618, 602, 118, 32, CLOSE, font,
    )?;
    let (tx, rx) = mpsc::channel();
    let installed = Installation::load().ok();
    let mut ui = Ui {
        window,
        install,
        uninstall,
        path,
        config,
        path_browse,
        config_browse,
        destination,
        status,
        percent,
        progress,
        log,
        detect,
        start,
        close,
        title_font,
        font,
        tx,
        rx,
        running: false,
        detecting: false,
        remove: false,
        steps: 0,
        installed,
    };
    if let Some(i) = &ui.installed {
        set_candidate(
            &ui,
            &Candidate {
                launcher: i.launcher.clone(),
                executable: i.securecrt_exe.clone(),
                config: i.config_dir.clone(),
            },
        );
        let version = i.version.clone();
        let skill = i.skill_dir.join("SKILL.md");
        append(
            &mut ui,
            &format!("检测到已安装版本 {version}，可升级或卸载。"),
        );
        append(&mut ui, &format!("已安装的 AI skill：{}", skill.display()));
    }
    if let Some(path) = crate::option(args, "--path") {
        set_text(ui.path, path);
        if let Some(c) = installer::candidate(Path::new(path)) {
            set_candidate(&ui, &c);
        }
    }
    if let Some(path) = crate::option(args, "--config") {
        set_text(ui.config, path);
    }
    UI.with(|c| *c.borrow_mut() = Some(ui));
    mode(args.first().is_some_and(|a| a == "uninstall"));
    SetTimer(Some(window), 1, 75, None);
    Ok(window)
}
pub fn run(args: &[String]) -> Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let result = (|| -> Result<()> {
            let window = create(args)?;
            let _ = ShowWindow(window, SW_SHOW);
            detect();
            let mut message = MSG::default();
            loop {
                let status = GetMessageW(&mut message, None, 0, 0).0;
                if status == 0 {
                    break;
                }
                if status < 0 {
                    return Err(windows::core::Error::from_thread().into());
                }
                if !IsDialogMessageW(window, &message).as_bool() {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            Ok(())
        })();
        UI.with(|cell| {
            if let Some(ui) = cell.borrow_mut().take() {
                let _ = DeleteObject(ui.font.into());
                let _ = DeleteObject(ui.title_font.into());
            }
        });
        CoUninitialize();
        result
    }
}
/// Hidden, read-only self-check of our own controls; no installer actions execute.
pub fn self_test() -> Result<Value> {
    unsafe {
        let window = create(&[])?;
        let result = (|| -> Result<Value> {
            UI.with(|c| -> Result<()> {
                let mut state = c.borrow_mut();
                let ui = state.as_mut().context("Setup window missing")?;
                append(ui, "界面自检：日志可滚动，进度可更新");
                send(ui.progress, PBM_SETPOS, 37, 0);
                anyhow::ensure!(
                    send(ui.progress, PBM_GETPOS, 0, 0).0 == 37,
                    "Progress control did not update"
                );
                anyhow::ensure!(
                    get_text(ui.log).contains("界面自检"),
                    "Log control did not update"
                );
                anyhow::ensure!(
                    get_text(ui.start) == "开始安装 / 升级",
                    "Wrong initial operation"
                );
                // In-memory fixture only; never write discovery, files or registry.
                if ui.installed.is_none() {
                    ui.installed = Some(Installation {
                        version: "test".into(),
                        install_dir: "C:/Example/SecureCRTBridge".into(),
                        securecrt_exe: "C:/Example/SecureCRT.exe".into(),
                        launcher: "C:/Example/SecureCRT.exe".into(),
                        config_dir: "C:/Example/Config".into(),
                        skill_dir: "C:/Example/skill".into(),
                    });
                }
                Ok(())
            })?;
            mode(true);
            UI.with(|c| -> Result<()> {
                let state = c.borrow();
                let ui = state.as_ref().unwrap();
                anyhow::ensure!(
                    get_text(ui.start) == "开始卸载",
                    "Uninstall mode label failed"
                );
                anyhow::ensure!(
                    !windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(ui.path)
                        .as_bool(),
                    "Uninstall target must be fixed"
                );
                Ok(())
            })?;
            mode(false);
            UI.with(|c| {
                let mut state = c.borrow_mut();
                let ui = state.as_mut().unwrap();
                ui.running = true;
                state_controls(ui);
                let _ = ui.tx.send(Event::Step(62, "组件注册完成".into()));
            });
            drain();
            UI.with(|c| -> Result<()> {
                let state = c.borrow();
                let ui = state.as_ref().unwrap();
                anyhow::ensure!(
                    send(ui.progress, PBM_GETPOS, 0, 0).0 == 62 && get_text(ui.percent) == "62%",
                    "Worker progress not rendered"
                );
                anyhow::ensure!(
                    !windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(ui.start)
                        .as_bool(),
                    "Double execution not prevented"
                );
                let _ = ui.tx.send(Event::Finished(Err("模拟路径错误".into())));
                Ok(())
            })?;
            drain();
            UI.with(|c| -> Result<()> {
                let state = c.borrow();
                let ui = state.as_ref().unwrap();
                anyhow::ensure!(
                    get_text(ui.log).contains("模拟路径错误"),
                    "Failure reason missing"
                );
                anyhow::ensure!(
                    windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(ui.start)
                        .as_bool(),
                    "Retry was not enabled"
                );
                Ok(())
            })?;
            Ok(
                json!({"ok":true,"progress_events":true,"uninstall_mode":true,"busy_controls":true,"error_and_retry":true,"mutations":0}),
            )
        })();
        let _ = DestroyWindow(window);
        UI.with(|cell| {
            if let Some(ui) = cell.borrow_mut().take() {
                let _ = DeleteObject(ui.font.into());
                let _ = DeleteObject(ui.title_font.into());
            }
        });
        result
    }
}
