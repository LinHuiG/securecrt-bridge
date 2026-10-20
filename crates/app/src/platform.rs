use anyhow::{bail, Context, Result};
use scb_core::w;
use std::path::{Path, PathBuf};
use windows::{
    core::{PCWSTR, PWSTR},
    Win32::{
        Foundation::*,
        Security::Authorization::*,
        Security::*,
        System::{Diagnostics::ToolHelp::*, Registry::*, Threading::*},
        UI::{Controls::Dialogs::*, WindowsAndMessaging::*},
    },
};

pub fn message(text: &str, error: bool) {
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(w(text).as_ptr()),
            PCWSTR(w("SecureCRT Bridge").as_ptr()),
            MB_OK
                | if error {
                    MB_ICONERROR
                } else {
                    MB_ICONINFORMATION
                },
        );
    }
}
pub fn choose_file(title: &str, filename: &str) -> Result<PathBuf> {
    let mut buffer = vec![0u16; 32768];
    let initial = w(filename);
    buffer[..initial.len()].copy_from_slice(&initial);
    let title = w(title);
    let filter = w("Windows executable / configuration\0*.exe;*.ini\0All files\0*.*\0");
    let mut dlg = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        lpstrFile: PWSTR(buffer.as_mut_ptr()),
        nMaxFile: buffer.len() as u32,
        lpstrTitle: PCWSTR(title.as_ptr()),
        lpstrFilter: PCWSTR(filter.as_ptr()),
        Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    if !unsafe { GetOpenFileNameW(&mut dlg) }.as_bool() {
        bail!("Selection cancelled");
    }
    let n = buffer.iter().position(|x| *x == 0).unwrap_or(buffer.len());
    Ok(PathBuf::from(String::from_utf16(&buffer[..n])?))
}
pub fn reg_get(root: HKEY, key: &str, name: &str) -> Option<String> {
    reg_get_checked(root, key, name).ok().flatten()
}
pub fn reg_get_checked(root: HKEY, key: &str, name: &str) -> Result<Option<String>> {
    unsafe {
        let mut data = vec![0u16; 32768];
        let mut size = (data.len() * 2) as u32;
        let status = RegGetValueW(
            root,
            PCWSTR(w(key).as_ptr()),
            PCWSTR(w(name).as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(data.as_mut_ptr().cast()),
            Some(&mut size),
        );
        if status == ERROR_FILE_NOT_FOUND || status == ERROR_PATH_NOT_FOUND {
            return Ok(None);
        }
        status
            .ok()
            .with_context(|| format!("无法读取注册表：{key} {name}"))?;
        let n = (size as usize / 2).saturating_sub(1);
        Ok(Some(String::from_utf16(&data[..n])?))
    }
}
pub fn reg_set(key: &str, name: &str, value: Option<&str>) -> Result<()> {
    unsafe {
        let mut h = HKEY::default();
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(w(key).as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut h,
            None,
        )
        .ok()?;
        let result = if let Some(v) = value {
            let bytes: Vec<u8> = w(v).into_iter().flat_map(u16::to_le_bytes).collect();
            RegSetValueExW(h, PCWSTR(w(name).as_ptr()), None, REG_SZ, Some(&bytes)).ok()
        } else {
            let r = RegDeleteValueW(h, PCWSTR(w(name).as_ptr()));
            if r == ERROR_FILE_NOT_FOUND {
                Ok(())
            } else {
                r.ok()
            }
        };
        let _ = RegCloseKey(h);
        result?;
        Ok(())
    }
}
pub fn private_directory(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
        let mut needed = 0;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut needed);
        let mut data = vec![0usize; (needed as usize + 7) / 8];
        let result = GetTokenInformation(
            token,
            TokenUser,
            Some(data.as_mut_ptr().cast()),
            needed,
            &mut needed,
        );
        let _ = CloseHandle(token);
        result?;
        let user = &*(data.as_ptr() as *const TOKEN_USER);
        let mut sid = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut sid)?;
        let sidtext = sid.to_string()?;
        LocalFree(Some(HLOCAL(sid.0.cast())));
        let sddl = w(format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;{sidtext})"));
        let mut sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )?;
        let mut present = windows::core::BOOL::default();
        let mut defaulted = windows::core::BOOL::default();
        let mut dacl = std::ptr::null_mut();
        let result = (|| -> Result<()> {
            GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted)?;
            SetNamedSecurityInfoW(
                PCWSTR(w(path).as_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(dacl),
                None,
            )
            .ok()?;
            Ok(())
        })();
        LocalFree(Some(HLOCAL(sd.0)));
        result
    }
}
pub struct Handle(pub HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
pub fn singleton() -> Result<Option<Handle>> {
    use sha2::{Digest, Sha256};
    let key = format!(
        "Local\\SecureCRTBridge-{:x}",
        Sha256::digest(
            scb_core::data_dir()
                .to_string_lossy()
                .to_lowercase()
                .as_bytes()
        )
    );
    let h = unsafe { CreateMutexW(None, false, PCWSTR(w(key).as_ptr())) }?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            let _ = CloseHandle(h);
        }
        return Ok(None);
    }
    Ok(Some(Handle(h)))
}
pub fn securecrt_processes() -> Result<Vec<u32>> {
    unsafe {
        let snap = Handle(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)?);
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut ids = Vec::new();
        if Process32FirstW(snap.0, &mut entry).is_ok() {
            loop {
                let name = String::from_utf16_lossy(
                    &entry.szExeFile[..entry.szExeFile.iter().position(|x| *x == 0).unwrap_or(260)],
                );
                if name.eq_ignore_ascii_case("SecureCRT.exe") {
                    ids.push(entry.th32ProcessID);
                }
                if Process32NextW(snap.0, &mut entry).is_err() {
                    break;
                }
            }
        }
        Ok(ids)
    }
}
pub fn validate_exe(path: &Path) -> Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("Read {}", path.display()))?;
    if bytes.len() < 64 || &bytes[..2] != b"MZ" {
        bail!("Not a Windows executable");
    }
    let offset = u32::from_le_bytes(bytes[60..64].try_into()?) as usize;
    if bytes.get(offset..offset + 4) != Some(b"PE\0\0")
        || bytes.get(offset + 4..offset + 6) != Some(&[0x64, 0x86])
    {
        bail!("This package requires 64-bit SecureCRT");
    }
    Ok(())
}
