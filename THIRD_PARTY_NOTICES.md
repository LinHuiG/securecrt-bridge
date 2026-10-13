# Third-party components

SecureCRT Bridge is an independent project, not affiliated with VanDyke Software or QiZhi.
SecureCRT and the bastion client are not included. Users need their own licensed SecureCRT installation.

Direct Rust dependencies, pinned in Cargo.lock:

| Component | License | Source |
| --- | --- | --- |
| anyhow | MIT OR Apache-2.0 | https://github.com/dtolnay/anyhow |
| serde / serde_json | MIT OR Apache-2.0 | https://github.com/serde-rs |
| uuid | Apache-2.0 OR MIT | https://github.com/uuid-rs/uuid |
| sha2 | MIT OR Apache-2.0 | https://github.com/RustCrypto/hashes |
| windows / windows-core | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |

Cargo packages and their transitive dependencies are compiled into the delivered binaries.
The installer does not redistribute SecureCRT, Rust, MinGW, Python, or Node.

Full notices for the cached Cargo packages are included in
`assets/THIRD_PARTY_LICENSES.txt` and copied into the installation directory.
The lockfile also lists `r-efi` for non-Windows targets; it is not linked into
the Windows build.
