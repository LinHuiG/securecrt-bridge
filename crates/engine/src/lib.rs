#![allow(non_snake_case)]
mod dispatch;
mod probe;
mod terminal;
use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use windows::{
    core::*,
    Win32::{
        Foundation::{
            CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_FAIL, E_NOTIMPL, E_POINTER,
        },
        System::{Com::*, Diagnostics::Debug::ActiveScript::*, Variant::VARIANT},
    },
};
const CLSID: GUID = GUID::from_u128(0x977cf644_46b4_42d1_acd2_bac5dcb5747c);
const PROBE_CLSID: GUID = GUID::from_u128(0x72599488_7997_4d79_9ef6_40130489082b);
static OBJECTS: AtomicUsize = AtomicUsize::new(0);
static LOCKS: AtomicUsize = AtomicUsize::new(0);
#[implement(IActiveScript, IActiveScriptParse64)]
struct Engine {
    site: RefCell<Option<IActiveScriptSite>>,
    state: Cell<SCRIPTSTATE>,
    code: RefCell<String>,
    started: Cell<bool>,
    stop: AtomicBool,
}
impl Engine {
    fn new() -> Self {
        OBJECTS.fetch_add(1, Ordering::SeqCst);
        Self {
            site: RefCell::new(None),
            state: Cell::new(SCRIPTSTATE_UNINITIALIZED),
            code: RefCell::new(String::new()),
            started: Cell::new(false),
            stop: AtomicBool::new(false),
        }
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        OBJECTS.fetch_sub(1, Ordering::SeqCst);
    }
}
impl Engine_Impl {
    fn run(&self) -> Result<()> {
        if self.started.get() || self.code.borrow().trim().is_empty() {
            return Ok(());
        }
        self.started.set(true);
        let site = self
            .site
            .borrow()
            .clone()
            .ok_or_else(|| Error::from_hresult(E_FAIL))?;
        let code = self.code.borrow().clone();
        let metadata: serde_json::Value = serde_json::from_str(&code)
            .map_err(|_| Error::new(E_FAIL, "Invalid bridge entry descriptor"))?;
        if metadata["mode"] == "probe" {
            unsafe {
                site.OnEnterScript()?;
            }
            let result = probe::run(&site, &metadata);
            unsafe {
                let _ = site.OnLeaveScript();
                let _ = site.OnScriptTerminate(std::ptr::null(), std::ptr::null());
            }
            return result.map_err(|_| {
                Error::new(
                    E_FAIL,
                    "Diagnostic probe failed; inspect its isolated report",
                )
            });
        }
        if let Some(path) = metadata["previous_script"].as_str() {
            if !path.is_empty() {
                unsafe { run_previous(&site, path) }?;
            }
        }
        unsafe {
            site.OnEnterScript()?;
        }
        let result = terminal::run(&site, &self.stop);
        unsafe {
            let _ = site.OnLeaveScript();
            let _ = site.OnScriptTerminate(std::ptr::null(), std::ptr::null());
        }
        if let Err(e) = result {
            terminal::diagnostic(&format!("Native engine stopped: {e:#}"));
            return Err(Error::new(
                E_FAIL,
                "SecureCRT Bridge stopped; see diagnostics.log",
            ));
        }
        Ok(())
    }
}
unsafe fn run_previous(site: &IActiveScriptSite, path: &str) -> Result<()> {
    let bytes = std::fs::read(path)
        .map_err(|_| Error::new(E_FAIL, "Original logon script could not be read"))?;
    let (text, _) = scb_core::config::decode(&bytes)
        .map_err(|_| Error::new(E_FAIL, "Original script encoding unsupported"))?;
    let lang = text
        .lines()
        .find(|l| l.trim_start().starts_with("#$language"))
        .and_then(|l| l.split('"').nth(1))
        .ok_or_else(|| Error::new(E_FAIL, "Original script has no language header"))?;
    if lang == scb_core::ENGINE_PROGID {
        return Err(Error::new(E_FAIL, "Recursive bridge script"));
    }
    let wlang = scb_core::w(lang);
    let clsid = CLSIDFromProgID(PCWSTR(wlang.as_ptr()))?;
    let engine: IActiveScript = CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER)?;
    engine.SetScriptSite(site)?;
    let parser: IActiveScriptParse64 = engine.cast()?;
    parser.InitNew()?;
    engine.AddNamedItem(
        w!("crt"),
        SCRIPTITEM_ISVISIBLE | SCRIPTITEM_ISSOURCE | SCRIPTITEM_GLOBALMEMBERS,
    )?;
    let body = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("#$"))
        .collect::<Vec<_>>()
        .join("\r\n");
    let wide = scb_core::w(body);
    let parsed = parser.ParseScriptText(
        PCWSTR(wide.as_ptr()),
        PCWSTR::null(),
        None,
        PCWSTR::null(),
        0,
        0,
        SCRIPTTEXT_ISVISIBLE,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
    );
    let result = parsed.and_then(|_| engine.SetScriptState(SCRIPTSTATE_CONNECTED));
    let _ = engine.Close();
    result
}
impl IActiveScript_Impl for Engine_Impl {
    fn SetScriptSite(&self, site: Ref<IActiveScriptSite>) -> Result<()> {
        *self.site.borrow_mut() = Some(site.ok()?.clone());
        Ok(())
    }
    fn GetScriptSite(&self, iid: *const GUID, out: *mut *mut c_void) -> Result<()> {
        if iid.is_null() || out.is_null() {
            return Err(E_POINTER.into());
        }
        let site = self.site.borrow();
        unsafe {
            site.as_ref()
                .ok_or_else(|| Error::from_hresult(E_FAIL))?
                .query(iid, out)
                .ok()
        }
    }
    fn SetScriptState(&self, state: SCRIPTSTATE) -> Result<()> {
        self.state.set(state);
        if let Some(site) = self.site.borrow().as_ref() {
            unsafe {
                site.OnStateChange(state)?;
            }
        }
        if state == SCRIPTSTATE_CONNECTED || state == SCRIPTSTATE_STARTED {
            self.run()?;
        } else if state == SCRIPTSTATE_CLOSED || state == SCRIPTSTATE_DISCONNECTED {
            self.stop.store(true, Ordering::Relaxed);
        }
        Ok(())
    }
    fn GetScriptState(&self) -> Result<SCRIPTSTATE> {
        Ok(self.state.get())
    }
    fn Close(&self) -> Result<()> {
        self.stop.store(true, Ordering::Relaxed);
        self.state.set(SCRIPTSTATE_CLOSED);
        self.site.borrow_mut().take();
        Ok(())
    }
    fn AddNamedItem(&self, _: &PCWSTR, _: u32) -> Result<()> {
        Ok(())
    }
    fn AddTypeLib(&self, _: *const GUID, _: u32, _: u32, _: u32) -> Result<()> {
        Ok(())
    }
    fn GetScriptDispatch(&self, _: &PCWSTR) -> Result<IDispatch> {
        Err(E_NOTIMPL.into())
    }
    fn GetCurrentScriptThreadID(&self) -> Result<u32> {
        Ok(unsafe { windows::Win32::System::Threading::GetCurrentThreadId() })
    }
    fn GetScriptThreadID(&self, id: u32) -> Result<u32> {
        Ok(id)
    }
    fn GetScriptThreadState(&self, _: u32) -> Result<SCRIPTTHREADSTATE> {
        Ok(
            if self.started.get() && !self.stop.load(Ordering::Relaxed) {
                SCRIPTTHREADSTATE_RUNNING
            } else {
                SCRIPTTHREADSTATE_NOTINSCRIPT
            },
        )
    }
    fn InterruptScriptThread(&self, _: u32, _: *const EXCEPINFO, _: u32) -> Result<()> {
        self.stop.store(true, Ordering::Relaxed);
        Ok(())
    }
    fn Clone(&self) -> Result<IActiveScript> {
        Err(E_NOTIMPL.into())
    }
}
impl IActiveScriptParse64_Impl for Engine_Impl {
    fn InitNew(&self) -> Result<()> {
        self.state.set(SCRIPTSTATE_INITIALIZED);
        Ok(())
    }
    fn AddScriptlet(
        &self,
        _: &PCWSTR,
        _: &PCWSTR,
        _: &PCWSTR,
        _: &PCWSTR,
        _: &PCWSTR,
        _: &PCWSTR,
        _: u64,
        _: u32,
        _: u32,
        _: *mut BSTR,
        _: *mut EXCEPINFO,
    ) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn ParseScriptText(
        &self,
        code: &PCWSTR,
        _: &PCWSTR,
        _: Ref<IUnknown>,
        _: &PCWSTR,
        _: u64,
        _: u32,
        _: u32,
        _: *mut VARIANT,
        _: *mut EXCEPINFO,
    ) -> Result<()> {
        if self.started.get() {
            return Err(Error::new(E_FAIL, "Only one entry descriptor is supported"));
        }
        let text = unsafe { code.to_string() }?;
        *self.code.borrow_mut() = text
            .lines()
            .filter(|l| !l.trim_start().starts_with("#$"))
            .collect::<Vec<_>>()
            .join("\n");
        if self.state.get() == SCRIPTSTATE_CONNECTED || self.state.get() == SCRIPTSTATE_STARTED {
            self.run()?;
        }
        Ok(())
    }
}
#[implement(IClassFactory)]
struct Factory;
impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<IUnknown>,
        iid: *const GUID,
        out: *mut *mut c_void,
    ) -> Result<()> {
        if iid.is_null() || out.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe {
            *out = std::ptr::null_mut();
        }
        if outer.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let engine: IActiveScript = Engine::new().into();
        unsafe { engine.query(iid, out).ok() }
    }
    fn LockServer(&self, lock: BOOL) -> Result<()> {
        if lock.as_bool() {
            LOCKS.fetch_add(1, Ordering::SeqCst);
        } else {
            let _ = LOCKS.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| v.checked_sub(1));
        }
        Ok(())
    }
}
#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    clsid: *const GUID,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    if clsid.is_null() || iid.is_null() || out.is_null() {
        return E_POINTER;
    }
    *out = std::ptr::null_mut();
    if *clsid != CLSID && *clsid != PROBE_CLSID {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    let factory: IClassFactory = Factory.into();
    factory.query(iid, out)
}
// A host may keep factory references; never unload their live vtables.
#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    HRESULT(1)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn factory_and_engine_lifecycle() {
        unsafe {
            let mut ptr = std::ptr::null_mut();
            assert_eq!(
                DllGetClassObject(&CLSID, &IClassFactory::IID, &mut ptr),
                HRESULT(0)
            );
            let factory = IClassFactory::from_raw(ptr);
            let engine: IActiveScript = factory.CreateInstance(None).unwrap();
            let parser: IActiveScriptParse64 = engine.cast().unwrap();
            parser.InitNew().unwrap();
            assert_eq!(engine.GetScriptState().unwrap(), SCRIPTSTATE_INITIALIZED);
            engine
                .InterruptScriptThread(0, std::ptr::null(), 0)
                .unwrap();
            engine.Close().unwrap();
            assert_eq!(engine.GetScriptState().unwrap(), SCRIPTSTATE_CLOSED);
        }
    }
}
