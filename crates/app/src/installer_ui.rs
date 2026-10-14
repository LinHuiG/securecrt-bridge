//! Native setup window. UI owns only presentation; all mutations stay in installer.
use crate::installer::{self, Candidate};
use crate::skills::{self, Client};
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
const SKILLS_ENABLE: usize = 110;
const SKILLS_LIST: usize = 111;
const SKILLS_ADD: usize = 112;
const SKILLS_EDIT: usize = 113;
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
    skills_enable: HWND,
    skills_list: HWND,
    skills_add: HWND,
    skills_edit: HWND,
    clients: Vec<Client>,
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
    enabled(ui.skills_enable, idle && !ui.remove);
    let skills_on = idle && !ui.remove && send(ui.skills_enable, BM_GETCHECK, 0, 0).0 == 1;
    for hwnd in [ui.skills_list, ui.skills_add, ui.skills_edit] {
        enabled(hwnd, skills_on);
    }
    set_text(
        ui.start,
        if ui.remove {
            "开始卸载"
        } else {
            "开始安装 / 升级"
        },
    );
}
fn populate_skills(ui: &Ui) {
    send(ui.skills_list, LVM_DELETEALLITEMS, 0, 0);
    for (index, client) in ui.clients.iter().enumerate() {
        let mut name = w(&client.name);
        let item = LVITEMW {
            mask: LVIF_TEXT,
            iItem: index as i32,
            pszText: windows::core::PWSTR(name.as_mut_ptr()),
            ..Default::default()
        };
        send(
            ui.skills_list,
            LVM_INSERTITEMW,
            0,
            &item as *const _ as isize,
        );
        for (column, value) in [
            (
                1,
                if client.detected {
                    "已发现 / 已配置".into()
                } else {
                    "未检测到".into()
                },
            ),
            (2, client.directory.join("SKILL.md").display().to_string()),
        ] {
            let mut value = w(value);
            let item = LVITEMW {
                iSubItem: column,
                pszText: windows::core::PWSTR(value.as_mut_ptr()),
                ..Default::default()
            };
            send(
                ui.skills_list,
                LVM_SETITEMTEXTW,
                index,
                &item as *const _ as isize,
            );
        }
        let item = LVITEMW {
            stateMask: LVIS_STATEIMAGEMASK,
            state: LIST_VIEW_ITEM_STATE_FLAGS(if client.selected { 2 << 12 } else { 1 << 12 }),
            ..Default::default()
        };
        send(
            ui.skills_list,
            LVM_SETITEMSTATE,
            index,
            &item as *const _ as isize,
        );
    }
}
fn chosen_skills(ui: &Ui) -> Result<Vec<std::path::PathBuf>> {
    if send(ui.skills_enable, BM_GETCHECK, 0, 0).0 != 1 {
        return Ok(vec![]);
    }
    skills::normalize(
        ui.clients
            .iter()
            .enumerate()
            .filter(|(i, _)| {
                send(
                    ui.skills_list,
                    LVM_GETITEMSTATE,
                    *i,
                    LVIS_STATEIMAGEMASK.0 as isize,
                )
                .0 >> 12
                    == 2
            })
            .map(|(_, c)| c.directory.clone())
            .collect(),
    )
}
fn browse_skill(add: bool) {
    let target = UI.with(|c| {
        c.borrow().as_ref().and_then(|ui| {
            if ui.running || ui.detecting || ui.remove {
                return None;
            }
            let index = send(
                ui.skills_list,
                LVM_GETNEXTITEM,
                usize::MAX,
                LVNI_SELECTED as isize,
            )
            .0;
            Some((ui.window, index))
        })
    });
    let Some((window, index)) = target else {
        return;
    };
    if !add && index < 0 {
        UI.with(|c| {
            if let Some(ui) = c.borrow().as_ref() {
                set_text(ui.status, "请先选中要修改目录的客户端行。");
            }
        });
        return;
    }
    let selected = choose_folder(
        window,
        "选择客户端的 skills 目录（自动添加 securecrt-bridge 子目录）",
    );
    UI.with(|c| {
        if let Some(ui) = c.borrow_mut().as_mut() {
            match selected {
                Ok(Some(root)) => {
                    for (i, client) in ui.clients.iter_mut().enumerate() {
                        client.selected = send(
                            ui.skills_list,
                            LVM_GETITEMSTATE,
                            i,
                            LVIS_STATEIMAGEMASK.0 as isize,
                        )
                        .0 >> 12
                            == 2;
                    }
                    let directory = Path::new(&root).join(skills::NAME);
                    if add {
                        if let Some(client) = ui
                            .clients
                            .iter_mut()
                            .find(|c| skills::path_eq(&c.directory, &directory))
                        {
                            client.selected = true;
                        } else {
                            ui.clients.push(Client {
                                name: "其他客户端".into(),
                                directory: directory.clone(),
                                detected: true,
                                selected: true,
                            });
                        }
                    } else if let Some(client) = ui.clients.get_mut(index as usize) {
                        client.directory = directory.clone();
                        client.selected = true;
                    }
                    populate_skills(ui);
                    append(
                        ui,
                        &format!("Skill 目标路径：{}", directory.join("SKILL.md").display()),
                    );
                }
                Ok(None) => {}
                Err(error) => append(ui, &format!("选择 Skill 目录失败：{error:#}")),
            }
        }
    });
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
        let selected = if ui.remove {
            vec![]
        } else {
            match chosen_skills(ui) {
                Ok(paths) => paths,
                Err(error) => {
                    set_text(ui.status, &format!("Skill 目录错误：{error:#}"));
                    return None;
                }
            }
        };
        ui.running = true;
        ui.steps = 0;
        set_text(ui.log, "");
        send(ui.progress, PBM_SETPOS, 0, 0);
        set_text(ui.percent, "0%");
        state_controls(ui);
        let mut args = if ui.remove {
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
        if !ui.remove {
            if selected.is_empty() {
                args.extend(["--skills".into(), "none".into()]);
            }
            for path in selected {
                args.extend(["--skill-dir".into(), path.display().to_string()]);
            }
        }
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
                    SKILLS_ENABLE => UI.with(|c| {
                        if let Some(ui) = c.borrow().as_ref() {
                            state_controls(ui);
                        }
                    }),
                    SKILLS_ADD => browse_skill(true),
                    SKILLS_EDIT => browse_skill(false),
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
            dwICC: ICC_PROGRESS_CLASS | ICC_LISTVIEW_CLASSES
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
        bottom: 825,
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
    let skills_enable = control(
        window,
        "BUTTON",
        "安装 AI Skill（默认勾选已检测到的客户端，可多选）",
        WS_TABSTOP | WS_GROUP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32),
        24,
        306,
        712,
        26,
        SKILLS_ENABLE,
        font,
    )?;
    send(skills_enable, BM_SETCHECK, 1, 0);
    let skills_list = control(
        window,
        "SysListView32",
        "",
        WS_TABSTOP | WS_BORDER | WINDOW_STYLE(LVS_REPORT | LVS_SHOWSELALWAYS),
        24,
        338,
        712,
        132,
        SKILLS_LIST,
        font,
    )?;
    send(
        skills_list,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        0,
        (LVS_EX_CHECKBOXES | LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER | LVS_EX_INFOTIP) as isize,
    );
    for (index, (title, width)) in [
        ("客户端", 170),
        ("检测状态", 138),
        ("最终 SKILL.md 路径（可横向滚动）", 680),
    ]
    .into_iter()
    .enumerate()
    {
        let mut title = w(title);
        let column = LVCOLUMNW {
            mask: LVCF_TEXT | LVCF_WIDTH,
            cx: width,
            pszText: windows::core::PWSTR(title.as_mut_ptr()),
            ..Default::default()
        };
        send(
            skills_list,
            LVM_INSERTCOLUMNW,
            index,
            &column as *const _ as isize,
        );
    }
    let skills_add = control(
        window,
        "BUTTON",
        "其他客户端 / 添加目录…",
        WS_TABSTOP,
        24,
        479,
        230,
        30,
        SKILLS_ADD,
        font,
    )?;
    let skills_edit = control(
        window,
        "BUTTON",
        "修改选中行目录…",
        WS_TABSTOP,
        270,
        479,
        190,
        30,
        SKILLS_EDIT,
        font,
    )?;
    control(
        window,
        "STATIC",
        "开始安装前请关闭 SecureCRT。",
        WINDOW_STYLE(0),
        475,
        483,
        261,
        24,
        0,
        font,
    )?;
    let status = control(
        window,
        "STATIC",
        "请选择路径，然后开始安装。",
        WINDOW_STYLE(SS_PATHELLIPSIS.0),
        24,
        524,
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
        524,
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
        556,
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
        594,
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
        622,
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
        782,
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
        782,
        184,
        32,
        START,
        font,
    )?;
    let close = control(
        window, "BUTTON", "关闭", WS_TABSTOP, 618, 782, 118, 32, CLOSE, font,
    )?;
    let (tx, rx) = mpsc::channel();
    let installed = Installation::load().ok();
    let clients = skills::discover(installed.as_ref());
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
        skills_enable,
        skills_list,
        skills_add,
        skills_edit,
        clients,
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
        let skill_paths = i.skill_paths();
        append(
            &mut ui,
            &format!("检测到已安装版本 {version}，可升级或卸载。"),
        );
        for skill in skill_paths {
            append(
                &mut ui,
                &format!("已安装的 AI Skill：{}", skill.join("SKILL.md").display()),
            );
        }
    }
    populate_skills(&ui);
    for client in ui.clients.clone().into_iter().filter(|c| c.selected) {
        append(
            &mut ui,
            &format!(
                "默认勾选 {}：{}",
                client.name,
                client.directory.join("SKILL.md").display()
            ),
        );
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
                anyhow::ensure!(
                    send(ui.skills_enable, BM_GETCHECK, 0, 0).0 == 1,
                    "Skill installation must default enabled"
                );
                let expected = skills::normalize(
                    ui.clients
                        .iter()
                        .filter(|c| c.selected)
                        .map(|c| c.directory.clone())
                        .collect(),
                )?;
                anyhow::ensure!(
                    chosen_skills(ui)? == expected,
                    "Detected clients must be checked by default"
                );
                // Exercise real checkbox controls and preserve their selections when toggled.
                send(ui.skills_enable, BM_SETCHECK, 0, 0);
                state_controls(ui);
                anyhow::ensure!(
                    chosen_skills(ui)?.is_empty(),
                    "Skip skills did not clear installation plan"
                );
                anyhow::ensure!(
                    !windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(ui.skills_list)
                        .as_bool(),
                    "Skill list must be disabled when skipped"
                );
                send(ui.skills_enable, BM_SETCHECK, 1, 0);
                state_controls(ui);
                anyhow::ensure!(
                    chosen_skills(ui)? == expected,
                    "Re-enabling skills lost selections"
                );
                let clients = ui.clients.clone();
                ui.clients = vec![
                    Client {
                        name: "Test A".into(),
                        directory: "C:/Example/A/securecrt-bridge".into(),
                        detected: true,
                        selected: true,
                    },
                    Client {
                        name: "Test B".into(),
                        directory: "C:/Example/B/securecrt-bridge".into(),
                        detected: true,
                        selected: true,
                    },
                ];
                populate_skills(ui);
                anyhow::ensure!(
                    chosen_skills(ui)?.len() == 2,
                    "Client checkboxes must support multiple selection"
                );
                let unchecked = LVITEMW {
                    stateMask: LVIS_STATEIMAGEMASK,
                    state: LIST_VIEW_ITEM_STATE_FLAGS(1 << 12),
                    ..Default::default()
                };
                send(
                    ui.skills_list,
                    LVM_SETITEMSTATE,
                    0,
                    &unchecked as *const _ as isize,
                );
                anyhow::ensure!(
                    chosen_skills(ui)? == vec![ui.clients[1].directory.clone()],
                    "Unchecking a client affected the wrong destination"
                );
                send(
                    ui.skills_list,
                    LVM_SETITEMSTATE,
                    1,
                    &unchecked as *const _ as isize,
                );
                anyhow::ensure!(
                    chosen_skills(ui)?.is_empty(),
                    "Unchecking all clients must skip skills"
                );
                ui.clients = clients;
                populate_skills(ui);
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
                        skill_dirs: None,
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
                anyhow::ensure!(
                    !windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(ui.skills_enable)
                        .as_bool(),
                    "Uninstall must not edit skill targets"
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
                json!({"ok":true,"progress_events":true,"uninstall_mode":true,"busy_controls":true,"error_and_retry":true,"skill_multi_select":true,"skill_skip_toggle":true,"mutations":0}),
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
