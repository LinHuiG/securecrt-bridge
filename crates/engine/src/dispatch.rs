use anyhow::{Context, Result};
use windows::{
    core::{GUID, PCWSTR},
    Win32::System::{Com::*, Variant::VARIANT},
};
#[derive(Clone)]
pub struct Dispatch(pub IDispatch);
impl Dispatch {
    fn invoke(&self, name: &str, mut args: Vec<VARIANT>, flags: DISPATCH_FLAGS) -> Result<VARIANT> {
        let name_w = scb_core::w(name);
        let mut id = 0;
        let mut result = VARIANT::default();
        unsafe {
            self.0
                .GetIDsOfNames(&GUID::zeroed(), &PCWSTR(name_w.as_ptr()), 1, 0, &mut id)
        }
        .with_context(|| format!("SecureCRT member {name}"))?;
        args.reverse();
        let mut named = -3i32;
        let params = DISPPARAMS {
            rgvarg: args.as_mut_ptr(),
            cArgs: args.len() as u32,
            rgdispidNamedArgs: if flags == DISPATCH_PROPERTYPUT {
                &mut named
            } else {
                std::ptr::null_mut()
            },
            cNamedArgs: if flags == DISPATCH_PROPERTYPUT { 1 } else { 0 },
        };
        unsafe {
            self.0.Invoke(
                id,
                &GUID::zeroed(),
                0,
                flags,
                &params,
                Some(&mut result),
                None,
                None,
            )
        }
        .with_context(|| format!("SecureCRT call {name}"))?;
        Ok(result)
    }
    pub fn get(&self, name: &str) -> Result<VARIANT> {
        self.invoke(name, vec![], DISPATCH_PROPERTYGET)
    }
    pub fn set(&self, name: &str, value: impl Into<VARIANT>) -> Result<()> {
        self.invoke(name, vec![value.into()], DISPATCH_PROPERTYPUT)?;
        Ok(())
    }
    pub fn call(&self, name: &str, args: Vec<VARIANT>) -> Result<VARIANT> {
        self.invoke(name, args, DISPATCH_METHOD)
    }
    pub fn obj(&self, name: &str) -> Result<Self> {
        Ok(Self(IDispatch::try_from(&self.get(name)?)?))
    }
    pub fn object_call(&self, name: &str, args: Vec<VARIANT>) -> Result<Self> {
        Ok(Self(IDispatch::try_from(&self.call(name, args)?)?))
    }
    pub fn text(&self, name: &str) -> Result<String> {
        string(&self.get(name)?)
    }
    pub fn int(&self, name: &str) -> Result<i32> {
        Ok(i32::try_from(&self.get(name)?)?)
    }
    pub fn boolean(&self, name: &str) -> Result<bool> {
        Ok(bool::try_from(&self.get(name)?)?)
    }
}
pub fn string(v: &VARIANT) -> Result<String> {
    if v.vt() != windows::Win32::System::Variant::VT_BSTR {
        anyhow::bail!("Expected terminal string");
    }
    Ok(unsafe { v.Anonymous.Anonymous.Anonymous.bstrVal.to_string() })
}
