use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Change {
    pub path: PathBuf,
    pub original: BTreeMap<String, Option<String>>,
    pub installed: BTreeMap<String, String>,
}

pub fn decode(bytes: &[u8]) -> Result<(String, bool)> {
    if bytes.starts_with(&[255, 254]) {
        if bytes.len() % 2 != 0 {
            bail!("Invalid UTF-16 config");
        }
        Ok((
            String::from_utf16(
                &bytes[2..]
                    .chunks_exact(2)
                    .map(|x| u16::from_le_bytes([x[0], x[1]]))
                    .collect::<Vec<_>>(),
            )?,
            true,
        ))
    } else {
        Ok((
            std::str::from_utf8(bytes.strip_prefix(&[239, 187, 191]).unwrap_or(bytes))?.to_owned(),
            false,
        ))
    }
}
pub fn encode(text: &str, utf16: bool) -> Vec<u8> {
    if utf16 {
        [
            vec![255, 254],
            text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        ]
        .concat()
    } else {
        text.as_bytes().to_vec()
    }
}
pub fn values(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|s| s.split_once('=').map(|(k, v)| (k.to_owned(), v.to_owned())))
        .collect()
}
pub fn patch(text: &str, updates: &BTreeMap<String, Option<String>>) -> String {
    let mut used = std::collections::BTreeSet::new();
    let mut result = Vec::new();
    for line in text.lines() {
        if let Some((key, _)) = line.split_once('=') {
            if let Some(value) = updates.get(key) {
                if used.insert(key.to_owned()) {
                    if let Some(v) = value {
                        result.push(format!("{key}={v}"));
                    }
                }
                continue;
            }
        }
        result.push(line.to_owned());
    }
    for (key, value) in updates {
        if !used.contains(key) {
            if let Some(v) = value {
                result.push(format!("{key}={v}"));
            }
        }
    }
    result.join("\r\n") + "\r\n"
}
pub fn install_patch(
    path: &Path,
    updates: BTreeMap<String, String>,
    backups: &Path,
) -> Result<Change> {
    let bytes =
        std::fs::read(path).with_context(|| format!("Read configuration {}", path.display()))?;
    let (text, utf16) = decode(&bytes)?;
    let existing = values(&text);
    let original = updates
        .keys()
        .map(|k| (k.clone(), existing.get(k).cloned()))
        .collect();
    std::fs::create_dir_all(backups)?;
    let hash = format!("{:x}", Sha256::digest(path.to_string_lossy().as_bytes()));
    let backup = backups.join(format!("{hash}.ini"));
    if !backup.exists() {
        std::fs::write(backup, &bytes)?;
    }
    let updated = patch(
        &text,
        &updates
            .iter()
            .map(|(k, v)| (k.clone(), Some(v.clone())))
            .collect(),
    );
    crate::atomic_write(path, &encode(&updated, utf16))?;
    Ok(Change {
        path: path.into(),
        original,
        installed: updates,
    })
}
pub fn restore(change: &Change) -> Result<Vec<String>> {
    let (text, utf16) = decode(&std::fs::read(&change.path)?)?;
    let current = values(&text);
    let mut updates = BTreeMap::new();
    let mut conflicts = Vec::new();
    for (key, ours) in &change.installed {
        if current.get(key) == Some(ours) {
            updates.insert(key.clone(), change.original.get(key).cloned().flatten());
        } else if current.get(key) != change.original.get(key).and_then(Option::as_ref) {
            conflicts.push(key.clone());
        }
    }
    crate::atomic_write(&change.path, &encode(&patch(&text, &updates), utf16))?;
    Ok(conflicts)
}
/// An explicitly requested reinstall can reapply a selectively restored hook.
/// Preserve the original journal and any unrelated or conflicting user settings.
pub fn reapply(change: &Change) -> Result<bool> {
    let (text, utf16) = decode(&std::fs::read(&change.path)?)?;
    let current = values(&text);
    for (key, ours) in &change.installed {
        if current.get(key) != Some(ours)
            && current.get(key) != change.original.get(key).and_then(Option::as_ref)
        {
            return Ok(false);
        }
    }
    if !change
        .installed
        .iter()
        .all(|(k, v)| current.get(k) == Some(v))
    {
        let updates = change
            .installed
            .iter()
            .map(|(k, v)| (k.clone(), Some(v.clone())))
            .collect();
        crate::atomic_write(&change.path, &encode(&patch(&text, &updates), utf16))?;
    }
    Ok(true)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_requested_keys_change() {
        let original = "S:\"Hostname\"=server\r\nD:\"Use Script File\"=00000000\r\n";
        let change = BTreeMap::from([("D:\"Use Script File\"".into(), Some("00000001".into()))]);
        let modified = patch(original, &change);
        assert!(modified.contains("Hostname\"=server"));
        assert!(!modified.contains("00000000"));
        assert_eq!(decode(&encode(&modified, true)).unwrap().0, modified);
    }
    #[test]
    fn uninstall_preserves_user_edits_and_restores_our_keys() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-fixtures")
            .join(crate::id());
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("Default.ini");
        std::fs::write(
            &file,
            "S:\"Hostname\"=server\r\nD:\"Use Script File\"=00000000\r\n",
        )
        .unwrap();
        let change = install_patch(
            &file,
            BTreeMap::from([
                ("D:\"Use Script File\"".into(), "00000001".into()),
                ("S:\"Script Filename V2\"".into(), "bridge.scb".into()),
            ]),
            &root.join("backups"),
        )
        .unwrap();
        let text = std::fs::read_to_string(&file)
            .unwrap()
            .replace("bridge.scb", "custom.vbs")
            .replace("=server", "=new-server");
        std::fs::write(&file, text).unwrap();
        let conflicts = restore(&change).unwrap();
        let after = std::fs::read_to_string(file).unwrap();
        assert_eq!(conflicts, vec!["S:\"Script Filename V2\""]);
        assert!(after.contains("=custom.vbs"));
        assert!(after.contains("=new-server"));
        assert!(after.contains("=00000000"));
    }
    #[test]
    fn repaired_hook_retains_original_backup_and_user_edits() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-fixtures")
            .join(crate::id());
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("Default.ini");
        std::fs::write(&file, "S:\"Hostname\"=server\r\n").unwrap();
        let change = install_patch(
            &file,
            BTreeMap::from([
                ("D:\"Use Script File\"".into(), "00000001".into()),
                ("S:\"Script Filename V2\"".into(), "bridge.scb".into()),
            ]),
            &root.join("backups"),
        )
        .unwrap();
        assert!(restore(&change).unwrap().is_empty());
        std::fs::write(&file, "S:\"Hostname\"=user-edited\r\n").unwrap();
        assert!(reapply(&change).unwrap());
        assert!(std::fs::read_to_string(&file)
            .unwrap()
            .contains("bridge.scb"));
        assert!(restore(&change).unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "S:\"Hostname\"=user-edited\r\n"
        );
        std::fs::write(&file, "S:\"Script Filename V2\"=user.vbs\r\n").unwrap();
        assert!(!reapply(&change).unwrap());
        assert!(std::fs::read_to_string(file).unwrap().contains("user.vbs"));
    }
}
