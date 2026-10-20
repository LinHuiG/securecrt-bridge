//! Console-subsystem entry point: preserves stdin under PowerShell and Codex PTYs.
use anyhow::{ensure, Context, Result};
use scb_core::{channel, http, Discovery};
fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.as_slice() == ["--help"] {
        println!("securecrt-session.exe SESSION_ID --owner CHAT_ID\nOne scoped JSON-lines channel over stdin/stdout. Send {{\"op\":\"close\"}} to exit.");
        return Ok(());
    }
    ensure!(
        args.len() == 3 && args[1] == "--owner",
        "Usage: securecrt-session.exe SESSION_ID --owner CHAT_ID"
    );
    let connection = Discovery::load().context("Bridge is not running")?;
    channel::run(
        &args[0],
        &args[2],
        &mut std::io::stdin().lock(),
        &mut std::io::stdout().lock(),
        |method, path, body| http::call(&connection, method, path, body),
    )
}
fn main() {
    if let Err(error) = run() {
        eprintln!(
            "{}",
            serde_json::json!({"ok":false,"error":format!("{error:#}")})
        );
        std::process::exit(1);
    }
}
