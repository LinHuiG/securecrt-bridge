//! A single, explicitly authorized terminal channel over newline-delimited JSON.
//! Session, owner and broker connection are fixed at process start, never read from input.
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Screen,
    Claim,
    Exec {
        command: String,
        #[serde(default = "default_timeout")]
        timeout: u64,
        request_id: Option<String>,
    },
    Input {
        text: String,
        request_id: Option<String>,
    },
    Result {
        request_id: String,
    },
    Interrupt {
        job_id: String,
        request_id: Option<String>,
    },
    Acknowledge,
    Close,
}
fn default_timeout() -> u64 {
    30
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn emit(out: &mut impl Write, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *out, value)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}
fn checked(value: Value) -> Result<Value> {
    ensure!(
        value["ok"] == true,
        "{}",
        value["error"].as_str().unwrap_or("Bridge request failed")
    );
    Ok(value)
}

pub fn run(
    session: &str,
    owner: &str,
    input: &mut impl BufRead,
    out: &mut impl Write,
    mut call: impl FnMut(&str, &str, &Value) -> Result<Value>,
) -> Result<()> {
    ensure!(valid_id(session), "Invalid session id");
    ensure!(!owner.is_empty() && owner.len() <= 128, "Invalid owner");
    let base = format!("/sessions/{session}");
    let identity = json!({"owner":owner});
    let screen = checked(call("GET", &format!("{base}/screen"), &json!({}))?)?;
    ensure!(screen["connected"] == true, "Session is offline");
    checked(call("POST", &format!("{base}/claim"), &identity)?)?;
    let result = (|| -> Result<()> {
        emit(
            out,
            &json!({"ok":true,"event":"ready","session":session,"owner":owner,"screen":screen,"protocol":"scb-session-v1"}),
        )?;
        loop {
            // Bound escaped JSON as well as the broker's decoded 32 KiB command.
            // Close on overflow; never interpret an oversized line's remainder.
            let mut bytes = Vec::new();
            let n = (&mut *input).take(262_145).read_until(b'\n', &mut bytes)?;
            if n == 0 {
                break;
            }
            ensure!(n <= 262_144, "Oversized channel request; channel closed");
            if bytes.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let response = (|| -> Result<Option<Value>> {
                let request: Request =
                    serde_json::from_slice(&bytes).context("Expected one JSON request per line")?;
                let (action, payload) = match request {
                    Request::Close => return Ok(None),
                    Request::Screen => {
                        return Ok(Some(call("GET", &format!("{base}/screen"), &json!({}))?))
                    }
                    Request::Claim => {
                        return Ok(Some(call("POST", &format!("{base}/claim"), &identity)?))
                    }
                    Request::Acknowledge => {
                        return Ok(Some(call(
                            "POST",
                            &format!("{base}/acknowledge"),
                            &identity,
                        )?))
                    }
                    Request::Result { request_id } => {
                        ensure!(valid_id(&request_id), "Invalid request id");
                        let result =
                            checked(call("GET", &format!("/jobs/{request_id}"), &json!({}))?)?;
                        ensure!(
                            result["session"] == session && result["owner"] == owner,
                            "Result belongs to another session or owner"
                        );
                        return Ok(Some(result));
                    }
                    Request::Exec {
                        command,
                        timeout,
                        request_id,
                    } => {
                        ensure!(
                            (1..=300).contains(&timeout),
                            "timeout must be 1..300 seconds"
                        );
                        ensure!(!command.contains('\0'), "Command contains NUL");
                        (
                            "exec",
                            json!({"command":command,"timeout_seconds":timeout,"request_id":request_id}),
                        )
                    }
                    Request::Input { text, request_id } => (
                        "input",
                        json!({"text":text,"request_id":request_id,"timeout_seconds":30}),
                    ),
                    Request::Interrupt { job_id, request_id } => {
                        ensure!(valid_id(&job_id), "Invalid job id");
                        (
                            "interrupt",
                            json!({"job_id":job_id,"request_id":request_id,"timeout_seconds":30}),
                        )
                    }
                };
                let mut payload = payload;
                if action != "interrupt" {
                    let text = payload[if action == "exec" { "command" } else { "text" }]
                        .as_str()
                        .unwrap();
                    ensure!(
                        !text.is_empty() && text.len() <= 32_768,
                        "Empty or oversized command"
                    );
                }
                let id = payload["request_id"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(crate::id);
                ensure!(valid_id(&id), "Invalid request id");
                payload["request_id"] = id.clone().into();
                payload["owner"] = owner.into();
                checked(call("POST", &format!("{base}/claim"), &identity)?)?;
                emit(
                    out,
                    &json!({"event":"dispatch","request_id":id,"session":session,"op":action}),
                )?;
                // Never retry a write when transport status is unknown.
                let response = match call("POST", &format!("{base}/{action}"), &payload) {
                    Ok(mut value) => {
                        value["request_id"] = id.into();
                        value
                    }
                    Err(e) => {
                        json!({"ok":false,"request_id":id,"error":format!("{e:#}"),"outcome":"unknown","next":"Query result; do not resend"})
                    }
                };
                Ok(Some(response))
            })();
            match response {
                Ok(None) => break,
                Ok(Some(value)) => emit(out, &value)?,
                Err(e) => emit(out, &json!({"ok":false,"error":format!("{e:#}")}))?,
            }
        }
        Ok(())
    })();
    // Release only our lease. Busy/unknown jobs deliberately keep their broker hold.
    let release = call("POST", &format!("{base}/release"), &identity);
    result?;
    emit(
        out,
        &json!({"event":"closed","session":session,"released":release.as_ref().is_ok_and(|v|v["ok"]==true),"release":release.unwrap_or_else(|e|json!({"ok":false,"error":format!("{e:#}")}))}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::State;
    fn actor(state: &mut State) {
        state.request(
            "POST",
            "/actor/poll",
            json!({"id":"tab","title":"fixture","connected":true,"screen":"$ "}),
        );
    }
    #[test]
    fn one_channel_handles_multiple_commands_and_releases() {
        let mut state = State::default();
        actor(&mut state);
        let input = b"{\"op\":\"exec\",\"command\":\"echo one\",\"request_id\":\"one\"}\n{\"op\":\"result\",\"request_id\":\"one\"}\n{\"op\":\"exec\",\"command\":\"echo two\",\"request_id\":\"two\"}\n{\"op\":\"close\"}\n";
        let mut out = Vec::new();
        run("tab", "chat", &mut &input[..], &mut out, |m, p, v| {
            actor(&mut state);
            let value = state.request(m, p, v.clone()).1;
            if p.ends_with("/exec") {
                actor(&mut state);
                state.request(
                    "POST",
                    "/actor/result",
                    json!({"id":v["request_id"],"session":"tab","result":{"exit_code":0}}),
                );
            }
            Ok(value)
        })
        .unwrap();
        let lines: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines[0]["event"], "ready");
        assert_eq!(lines.iter().filter(|v| v["event"] == "dispatch").count(), 2);
        assert_eq!(lines[2]["ok"], true);
        assert_eq!(lines[5]["ok"], true);
        assert_eq!(lines.last().unwrap()["released"], true);
    }
    #[test]
    fn rejects_scope_override_and_keeps_unknown_hold_on_eof() {
        let mut state = State::default();
        actor(&mut state);
        let input = b"{\"op\":\"exec\",\"session\":\"other\",\"command\":\"bad\"}\n{\"op\":\"exec\",\"command\":\"once\",\"request_id\":\"one\"}\n";
        let mut writes = 0;
        let mut out = Vec::new();
        run("tab", "chat", &mut &input[..], &mut out, |m, p, v| {
            let value = state.request(m, p, v.clone()).1;
            if p.ends_with("/exec") {
                writes += 1;
                anyhow::bail!("Lost response");
            }
            Ok(value)
        })
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(writes, 1);
        assert!(text.contains("unknown field"));
        assert!(text.contains("\"outcome\":\"unknown\""));
        assert!(text.contains("\"released\":false"));
    }
    #[test]
    fn other_sessions_results_and_invalid_paths_are_rejected() {
        let mut out = Vec::new();
        let mut calls = 0;
        assert!(run("../tab", "chat", &mut &b""[..], &mut out, |_, _, _| {
            calls += 1;
            Ok(json!({}))
        })
        .is_err());
        assert_eq!(calls, 0);
        let input = b"{\"op\":\"result\",\"request_id\":\"foreign\"}\n";
        run("tab", "chat", &mut &input[..], &mut out, |_, p, _| {
            Ok(if p.starts_with("/jobs/") {
                json!({"ok":true,"session":"other","owner":"chat","result":"secret"})
            } else {
                json!({"ok":true,"connected":true})
            })
        })
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("another session"));
        assert!(!text.contains("secret"));
    }
}
