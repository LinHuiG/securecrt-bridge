fn main() {
    println!("cargo:rerun-if-env-changed=SCB_ENGINE_DLL");
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let text = if let Ok(path) = std::env::var("SCB_ENGINE_DLL") {
        println!("cargo:rerun-if-changed={path}");
        format!("pub static ENGINE: &[u8] = include_bytes!({path:?});")
    } else {
        "pub static ENGINE: &[u8] = &[];".into()
    };
    std::fs::write(out.join("payload.rs"), text).unwrap();
    let crate_dir = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let manifest = crate_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("assets/setup.manifest");
    println!("cargo:rerun-if-changed={}", manifest.display());
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
            manifest.display()
        );
    } else {
        println!("cargo:rerun-if-env-changed=SCB_WINDRES");
        let rc = out.join("setup.rc");
        let object = out.join("setup-resource.o");
        let file = manifest
            .to_string_lossy()
            .trim_start_matches("\\\\?\\")
            .replace('\\', "/");
        std::fs::write(&rc, format!("1 24 \"{file}\"\n")).unwrap();
        let tool = std::env::var_os("SCB_WINDRES").unwrap_or_else(|| "windres".into());
        let status = std::process::Command::new(tool)
            .args([
                "--input-format=rc",
                "--output-format=coff",
                "--target=pe-x86-64",
            ])
            .arg(&rc)
            .arg(&object)
            .status()
            .expect("GNU builds require windres on PATH (or SCB_WINDRES)");
        assert!(status.success(), "windres failed");
        println!("cargo:rustc-link-arg-bins={}", object.display());
    }
}
