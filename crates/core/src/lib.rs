pub mod broker;
pub mod config;
pub mod http;
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const ENGINE_PROGID: &str = "SecureCRTBridge.Native";
pub const ENGINE_CLSID: &str = "{977CF644-46B4-42D1-ACD2-BAC5DCB5747C}";

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub fn id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}
pub fn data_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("SCB_DATA_DIR") {
        return p.into();
    }
    PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default())
        .join("AppData/Local/SecureCRTBridge")
}
pub fn w(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    s.as_ref().encode_wide().chain(Some(0)).collect()
}
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use windows::{
        core::PCWSTR,
        Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        },
    };
    std::fs::create_dir_all(path.parent().context("No parent directory")?)?;
    let tmp = path.with_extension(format!("{}.tmp", id()));
    std::fs::write(&tmp, bytes)?;
    let source = w(&tmp);
    let target = w(path);
    let mut last = None;
    for _ in 0..10 {
        match unsafe {
            MoveFileExW(
                PCWSTR(source.as_ptr()),
                PCWSTR(target.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } {
            Ok(()) => return Ok(()),
            Err(e) => last = Some(e),
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = std::fs::remove_file(tmp);
    Err(last.unwrap().into())
}
pub fn save_json(path: &Path, value: &impl Serialize) -> Result<()> {
    atomic_write(path, &serde_json::to_vec_pretty(value)?)
}
pub fn load_json<T: for<'a> Deserialize<'a>>(path: &Path) -> Result<T> {
    Ok(serde_json::from_slice(
        &std::fs::read(path).with_context(|| format!("Read {}", path.display()))?,
    )?)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Discovery {
    pub version: String,
    pub port: u16,
    pub token: String,
    pub epoch: String,
    pub pid: u32,
}
impl Discovery {
    pub fn load() -> Result<Self> {
        load_json(&data_dir().join("connection.json"))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Installation {
    pub version: String,
    pub install_dir: PathBuf,
    pub securecrt_exe: PathBuf,
    pub launcher: PathBuf,
    pub config_dir: PathBuf,
    pub skill_dir: PathBuf,
}
impl Installation {
    pub fn load() -> Result<Self> {
        load_json(&data_dir().join("installation.json"))
    }
}

pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let mut result = a.len() ^ b.len();
    for (x, y) in a.as_bytes().iter().zip(b.as_bytes()) {
        result |= (x ^ y) as usize;
    }
    result == 0
}
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}
pub fn frame_command(command: &str, request_id: &str) -> (String, String, String) {
    let begin = format!("__SCB_{}_BEGIN__", request_id);
    let end = format!("__SCB_{}_END__", request_id);
    let split = |s: &str| format!("{}{}", shell_quote(&s[..6]), shell_quote(&s[6..]));
    (format!("(printf '\\n%s\\n' {}; sh -c {}; __scb_rc=$?; printf '\\n%s:%s\\n' {} \"$__scb_rc\")\r",split(&begin),shell_quote(command),split(&end)),begin,end)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framing_cannot_match_command_echo() {
        let (text, begin, end) = frame_command("printf '%s' \"a'b\"", &id());
        assert!(!text.contains(&begin));
        assert!(!text.contains(&end));
        assert!(text.contains("sh -c"));
        assert!(text.ends_with('\r'));
    }
    #[test]
    fn token_comparison() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
    }
}
