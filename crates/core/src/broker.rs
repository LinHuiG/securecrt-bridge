//! In-memory session registry. Delivery changes state BEFORE returning the job.
//! A lost response must be queried by request id, never implicitly re-executed.
use crate::{http, now_ms, Discovery};
use anyhow::Result;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

#[derive(Default)]
pub struct State {
    pub paused: bool,
    pub shutdown: bool,
    sessions: BTreeMap<String, Session>,
    jobs: BTreeMap<String, Job>,
    preferences: Option<std::path::PathBuf>,
}
#[derive(Default)]
struct Session {
    title: String,
    screen: String,
    connected: bool,
    last_seen: u64,
    paused: bool,
    owner: String,
    lease_until: u64,
    busy: Option<String>,
    interrupt: Option<String>,
}
struct Job {
    session: String,
    owner: String,
    payload: Value,
    status: String,
    result: Value,
    deadline: u64,
    updated: u64,
    submitted: u64,
    started: Option<u64>,
}
pub type Shared = Arc<Mutex<State>>;
fn fail(code: u16, message: &str) -> (u16, Value) {
    (code, json!({"ok":false,"error":message}))
}
fn ok(value: Value) -> (u16, Value) {
    (200, value)
}
fn field<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}
fn sid_valid(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 80
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl State {
    pub fn persist_preferences_at(&mut self, path: std::path::PathBuf) {
        self.preferences = Some(path);
    }
    pub fn request(&mut self, method: &str, path: &str, v: Value) -> (u16, Value) {
        let now = now_ms();
        for job in self.jobs.values_mut() {
            if job.status == "queued" && now > job.deadline {
                job.status = "expired".into();
                job.updated = now;
            }
            if job.status == "running" && now > job.deadline + 10_000 {
                job.status = "unknown".into();
                job.updated = now;
            }
        }
        // Keep a small tombstone for every id during this service lifetime.
        // Expiring output must not turn an old request into a new command.
        for j in self.jobs.values_mut() {
            if now.saturating_sub(j.updated) > 30 * 60 * 1000 && j.status != "unknown" {
                j.payload = Value::Null;
                j.result = Value::Null;
                j.status = "history_expired".into();
            }
        }
        for s in self.sessions.values_mut() {
            if s.interrupt.as_ref().is_some_and(|id| {
                self.jobs
                    .get(id)
                    .is_none_or(|j| !matches!(j.status.as_str(), "queued" | "running" | "unknown"))
            }) {
                s.interrupt = None;
            }
            if s.busy.as_ref().is_some_and(|id| {
                self.jobs.get(id).is_none_or(|j| {
                    j.status != "queued" && j.status != "running" && j.status != "unknown"
                })
            }) {
                s.busy = None;
            }
        }
        self.sessions
            .retain(|_, s| now.saturating_sub(s.last_seen) < 24 * 60 * 60 * 1000);
        if method == "GET" && path == "/health" {
            return ok(json!({"ok":true,"version":crate::VERSION,"paused":self.paused}));
        }
        if method == "POST" && path == "/shutdown" {
            // Stopping delivery is transient; do not overwrite the user's pause
            // preference and accidentally keep every subsequent launch paused.
            self.shutdown = true;
            for job in self.jobs.values_mut() {
                if job.status == "queued" {
                    job.status = "cancelled".into();
                }
            }
            return ok(json!({"ok":true}));
        }
        if self.shutdown && method == "POST" {
            return fail(503, "Service is stopping");
        }
        if method == "GET" && path == "/sessions" {
            return ok(
                json!({"ok":true,"paused":self.paused,"sessions":self.sessions.iter().map(|(id,s)|json!({"id":id,"title":s.title,"connected":s.connected&&now.saturating_sub(s.last_seen)<5000,"last_seen":s.last_seen,"paused":s.paused||self.paused,"owner":if s.lease_until>now{&s.owner}else{""},"busy":s.busy})).collect::<Vec<_>>()}),
            );
        }
        if method == "POST" && path == "/pause" {
            let paused = v["paused"].as_bool().unwrap_or(true);
            if let Some(path) = &self.preferences {
                if crate::save_json(path, &json!({"paused":paused})).is_err() {
                    return fail(500, "Cannot save pause preference");
                }
            }
            self.paused = paused;
            if self.paused {
                for j in self.jobs.values_mut() {
                    if j.status == "queued" {
                        j.status = "cancelled".into();
                    }
                }
            }
            return ok(json!({"ok":true,"paused":self.paused}));
        }
        if method == "POST" && path == "/actor/poll" {
            let id = field(&v, "id");
            if !sid_valid(id) {
                return fail(400, "Invalid session id");
            }
            if self.sessions.len() >= 512 && !self.sessions.contains_key(id) {
                return fail(429, "Too many sessions");
            }
            let s = self.sessions.entry(id.to_owned()).or_default();
            s.title = field(&v, "title").chars().take(512).collect();
            s.connected = v["connected"].as_bool().unwrap_or(false);
            s.last_seen = now;
            s.screen = field(&v, "screen").chars().take(150_000).collect();
            // An actor restarts with a new id. An unknown job holds the old session
            // busy until its actor explicitly reports a result or reconnects.
            if let Some(jid) = s.interrupt.as_ref().or(s.busy.as_ref()) {
                if let Some(j) = self.jobs.get_mut(jid) {
                    if j.status == "queued" && !s.paused && !self.paused && s.connected {
                        j.status = "running".into();
                        j.updated = now;
                        j.started = Some(now);
                        return ok(
                            json!({"ok":true,"job":{"id":jid,"payload":j.payload,"deadline":j.deadline}}),
                        );
                    }
                }
            }
            return ok(json!({"ok":true,"job":null}));
        }
        if method == "POST" && path == "/actor/result" {
            let jid = field(&v, "id");
            let Some(j) = self.jobs.get_mut(jid) else {
                return fail(404, "Job missing; do not repeat command");
            };
            if j.session != field(&v, "session") {
                return fail(409, "Wrong session");
            }
            if matches!(j.status.as_str(), "running" | "unknown") {
                j.result = v["result"].clone();
                j.status = if j.result["outcome"] == "unknown" {
                    "unknown"
                } else {
                    "done"
                }
                .into();
                j.updated = now;
                if j.status == "done" {
                    if let Some(s) = self.sessions.get_mut(&j.session) {
                        // A recovery keystroke finishing does not establish that
                        // the original shell is idle. Keep its hold until ack.
                        if s.busy.as_deref() == Some(jid) {
                            s.busy = None;
                            // A late original result makes an undelivered Ctrl+C
                            // unnecessary. Never let it hit a subsequent command.
                            if let Some(recovery) =
                                s.interrupt.as_ref().and_then(|id| self.jobs.get_mut(id))
                            {
                                if recovery.status == "queued" {
                                    recovery.status = "cancelled".into();
                                    recovery.updated = now;
                                    s.interrupt = None;
                                }
                            }
                        } else if s.interrupt.as_deref() == Some(jid) {
                            s.interrupt = None;
                        }
                    }
                }
            }
            return ok(json!({"ok":true}));
        }
        let p: Vec<_> = path.trim_matches('/').split('/').collect();
        if method == "GET" && p.len() == 2 && p[0] == "jobs" {
            return match self.jobs.get(p[1]) {
                Some(j) => ok(
                    json!({"ok":true,"id":p[1],"session":j.session,"owner":j.owner,"status":j.status,"result":j.result,"submitted_at":j.submitted,"started_at":j.started,"updated_at":j.updated}),
                ),
                None => fail(
                    404,
                    "Unknown request id; outcome may be unknown after restart",
                ),
            };
        }
        if p.len() != 3 || p[0] != "sessions" {
            return fail(404, "Unknown endpoint");
        }
        let id = p[1];
        let action = p[2];
        if method == "POST" && action == "interrupt" {
            return self.interrupt(id, &v, now);
        }
        let Some(s) = self.sessions.get_mut(id) else {
            return fail(404, "Session not found");
        };
        if method == "GET" && action == "screen" {
            return ok(
                json!({"ok":true,"id":id,"title":s.title,"screen":s.screen,"last_seen":s.last_seen,"connected":s.connected&&now.saturating_sub(s.last_seen)<5000}),
            );
        }
        if method != "POST" {
            return fail(405, "Use POST");
        }
        if action == "pause" {
            s.paused = v["paused"].as_bool().unwrap_or(true);
            if s.paused {
                for jid in [s.busy.as_ref(), s.interrupt.as_ref()]
                    .into_iter()
                    .flatten()
                {
                    if let Some(j) = self.jobs.get_mut(jid) {
                        if j.status == "queued" {
                            j.status = "cancelled".into();
                        }
                    }
                }
            }
            return ok(json!({"ok":true,"paused":s.paused}));
        }
        let owner = field(&v, "owner");
        if owner.is_empty() || owner.len() > 128 {
            return fail(400, "Provide a stable owner for this chat");
        }
        if action == "acknowledge" {
            if s.owner != owner {
                return fail(409, "Unresolved operation belongs to another owner");
            }
            let mut recovery_acknowledged = false;
            if let Some(recovery) = s.interrupt.as_ref().and_then(|id| self.jobs.get_mut(id)) {
                if matches!(recovery.status.as_str(), "queued" | "running") {
                    return fail(
                        409,
                        "Recovery input is still pending; query its request first",
                    );
                }
                if recovery.status == "unknown" {
                    recovery.status = "acknowledged".into();
                    recovery.updated = now;
                    recovery_acknowledged = true;
                }
                s.interrupt = None;
            }
            let Some(j) = s.busy.as_ref().and_then(|id| self.jobs.get_mut(id)) else {
                if recovery_acknowledged {
                    return ok(json!({"ok":true}));
                }
                return fail(409, "No unresolved operation");
            };
            if j.status != "unknown" {
                return fail(409, "Operation has not reached an unknown outcome");
            }
            j.status = "acknowledged".into();
            s.busy = None;
            return ok(json!({"ok":true}));
        }
        if action == "release" {
            if s.owner != owner {
                return fail(409, "Lease belongs to another owner");
            }
            if s.busy.is_some() || s.interrupt.is_some() {
                return fail(409, "Session still busy");
            }
            s.lease_until = 0;
            s.owner.clear();
            return ok(json!({"ok":true}));
        }
        if !s.connected || now.saturating_sub(s.last_seen) > 5000 {
            return fail(409, "Session is offline");
        }
        if s.owner != owner && s.lease_until > now {
            return fail(409, "Session is held by another chat");
        }
        if action == "claim" {
            if (s.busy.is_some() || s.interrupt.is_some()) && s.owner != owner {
                return fail(409, "Previous operation is unresolved");
            }
            s.owner = owner.into();
            s.lease_until = now + v["ttl_seconds"].as_u64().unwrap_or(120).clamp(10, 600) * 1000;
            return ok(json!({"ok":true,"owner":s.owner,"expires":s.lease_until}));
        }
        if action != "exec" && action != "input" {
            return fail(404, "Unknown session operation");
        }
        let request_id = field(&v, "request_id");
        if !sid_valid(request_id) {
            return fail(400, "Provide an opaque unique request_id");
        }
        if let Some(j) = self.jobs.get(request_id) {
            return if j.owner == owner && j.session == id && j.payload == v {
                ok(json!({"ok":true,"id":request_id,"status":j.status,"duplicate":true}))
            } else {
                fail(409, "request_id collision")
            };
        }
        if self.paused || s.paused {
            return fail(409, "Bridge is paused");
        }
        if self.jobs.len() >= 100_000 {
            return fail(
                429,
                "Request history is full; restart service after resolving active operations",
            );
        }
        if s.owner != owner || s.lease_until <= now {
            return fail(409, "Claim the session before writing");
        }
        if s.busy.is_some() || s.interrupt.is_some() {
            return fail(409, "Session busy; query previous request");
        }
        let command = field(&v, if action == "exec" { "command" } else { "text" });
        if command.len() > 32_768 || command.is_empty() {
            return fail(400, "Empty or oversized command");
        }
        if action == "exec" && command.contains('\0') {
            return fail(400, "Command contains NUL");
        }
        let deadline = now + v["timeout_seconds"].as_u64().unwrap_or(30).clamp(1, 300) * 1000;
        s.lease_until = s.lease_until.max(deadline + 10_000);
        s.busy = Some(request_id.into());
        self.jobs.insert(
            request_id.into(),
            Job {
                session: id.into(),
                owner: owner.into(),
                payload: v.clone(),
                status: "queued".into(),
                result: Value::Null,
                deadline,
                updated: now,
                submitted: now,
                started: None,
            },
        );
        ok(json!({"ok":true,"id":request_id,"status":"queued"}))
    }

    /// Queue exactly one Ctrl+C for a named unresolved operation. This uses the
    /// existing actor input protocol and works with already-loaded 0.1.5 DLLs.
    fn interrupt(&mut self, id: &str, v: &Value, now: u64) -> (u16, Value) {
        let owner = field(v, "owner");
        let request_id = field(v, "request_id");
        let original = field(v, "job_id");
        if owner.is_empty() || owner.len() > 128 || !sid_valid(request_id) || !sid_valid(original) {
            return fail(400, "Provide owner, unique request_id and original job_id");
        }
        let Some(s) = self.sessions.get_mut(id) else {
            return fail(404, "Session not found");
        };
        if s.owner != owner {
            return fail(409, "Unresolved operation belongs to another owner");
        }
        let payload =
            json!({"owner":owner,"request_id":request_id,"recovery_for":original,"text":"\u{3}"});
        if let Some(j) = self.jobs.get(request_id) {
            return if j.session == id && j.owner == owner && j.payload == payload {
                ok(json!({"ok":true,"id":request_id,"status":j.status,"duplicate":true}))
            } else {
                fail(409, "request_id collision")
            };
        }
        if self.paused || s.paused {
            return fail(409, "Bridge is paused");
        }
        if !s.connected || now.saturating_sub(s.last_seen) > 5000 {
            return fail(
                409,
                "Session is offline; wait for actor to finish capturing output",
            );
        }
        if s.lease_until <= now {
            return fail(409, "Renew the session lease before recovery");
        }
        if s.interrupt.is_some() {
            return fail(409, "Recovery input is unresolved; query its request first");
        }
        if s.busy.as_deref() != Some(original)
            || !self
                .jobs
                .get(original)
                .is_some_and(|j| j.owner == owner && j.status == "unknown")
        {
            return fail(409, "Recovery requires your current unknown job; it cannot interrupt a running or different job");
        }
        if self.jobs.len() >= 100_000 {
            return fail(429, "Request history is full");
        }
        let deadline = now + 15_000;
        s.lease_until = s.lease_until.max(deadline + 10_000);
        s.interrupt = Some(request_id.into());
        self.jobs.insert(
            request_id.into(),
            Job {
                session: id.into(),
                owner: owner.into(),
                payload,
                status: "queued".into(),
                result: Value::Null,
                deadline,
                updated: now,
                submitted: now,
                started: None,
            },
        );
        ok(json!({"ok":true,"id":request_id,"status":"queued","job_id":original}))
    }
}

pub fn serve(
    listener: TcpListener,
    cfg: Discovery,
    state: Shared,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    listener.set_nonblocking(true)?;
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    while !stop.load(Ordering::Relaxed) {
        if state.lock().unwrap_or_else(|e| e.into_inner()).shutdown {
            break;
        }
        match listener.accept() {
            Ok((mut stream, peer)) => {
                if !peer.ip().is_loopback() || active.load(Ordering::Relaxed) >= 64 {
                    continue;
                }
                active.fetch_add(1, Ordering::Relaxed);
                let cfg = cfg.clone();
                let state = state.clone();
                let active = active.clone();
                std::thread::spawn(move || {
                    let _ = handle(&mut stream, &cfg, &state);
                    active.fetch_sub(1, Ordering::Relaxed);
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
fn handle(stream: &mut TcpStream, cfg: &Discovery, state: &Shared) -> Result<()> {
    // Windows accepted sockets inherit the listener's nonblocking mode. Each
    // connection has its own worker, so use blocking reads with bounded timeouts.
    // Otherwise a fragmented request becomes WouldBlock and is closed mid-frame.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    let m = match http::read(stream) {
        Ok(m) => m,
        Err(_) => {
            return http::respond(
                stream,
                400,
                &json!({"ok":false,"error":"Malformed request"}),
            )
        }
    };
    let auth = m
        .headers
        .get("authorization")
        .and_then(|a| a.strip_prefix("Bearer "))
        .unwrap_or("");
    if !crate::constant_time_eq(auth, &cfg.token) {
        return http::respond(stream, 401, &json!({"ok":false,"error":"Unauthorized"}));
    }
    if m.headers.contains_key("origin") {
        return http::respond(
            stream,
            403,
            &json!({"ok":false,"error":"Browser origins are not supported"}),
        );
    }
    let parts: Vec<_> = m.first.split_whitespace().collect();
    if parts.len() != 3 {
        return http::respond(stream, 400, &json!({"ok":false}));
    }
    let value = if m.body.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice(&m.body) {
            Ok(v) => v,
            Err(_) => {
                return http::respond(stream, 400, &json!({"ok":false,"error":"Invalid JSON"}))
            }
        }
    };
    let (status, result) = state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .request(parts[0], parts[1], value);
    http::respond(stream, status, &result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepted_connection_waits_for_fragmented_request() {
        use std::io::Write;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let cfg = Discovery {
            version: crate::VERSION.into(),
            port,
            token: crate::id(),
            epoch: crate::id(),
            pid: std::process::id(),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let server_stop = stop.clone();
        let server_cfg = cfg.clone();
        let server = std::thread::spawn(move || {
            serve(
                listener,
                server_cfg,
                Arc::new(Mutex::new(State::default())),
                server_stop,
            )
        });
        let result = (|| -> Result<serde_json::Value> {
            let mut stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))?;
            stream.set_read_timeout(Some(Duration::from_secs(3)))?;
            stream.write_all(b"GET /health HTTP/1.1\r\n")?;
            std::thread::sleep(Duration::from_millis(100));
            write!(
                stream,
                "Authorization: Bearer {}\r\nContent-Length: 0\r\n\r\n",
                cfg.token
            )?;
            let reply = http::read(&mut stream)?;
            Ok(serde_json::from_slice(&reply.body)?)
        })();
        stop.store(true, Ordering::Relaxed);
        server.join().unwrap().unwrap();
        assert_eq!(result.unwrap()["ok"], true);
    }
    fn actor(s: &mut State) {
        s.request(
            "POST",
            "/actor/poll",
            json!({"id":"tab","title":"test","connected":true,"screen":"$ "}),
        );
    }
    #[test]
    fn exclusive_at_most_once_delivery() {
        let mut s = State::default();
        actor(&mut s);
        assert_eq!(
            s.request("POST", "/sessions/tab/claim", json!({"owner":"one"}))
                .0,
            200
        );
        assert_eq!(
            s.request("POST", "/sessions/tab/claim", json!({"owner":"two"}))
                .0,
            409
        );
        let payload = json!({"owner":"one","request_id":"a","command":"pwd"});
        assert_eq!(
            s.request("POST", "/sessions/tab/exec", payload.clone()).0,
            200
        );
        assert!(s
            .request("POST", "/actor/poll", json!({"id":"tab","connected":true}))
            .1["job"]
            .is_object());
        assert!(s
            .request("POST", "/actor/poll", json!({"id":"tab","connected":true}))
            .1["job"]
            .is_null());
        assert_eq!(
            s.request("POST", "/sessions/tab/exec", payload).1["duplicate"],
            true
        );
        assert_eq!(
            s.request(
                "POST",
                "/sessions/tab/input",
                json!({"owner":"one","request_id":"b","text":"x"})
            )
            .0,
            409
        );
        s.request(
            "POST",
            "/actor/result",
            json!({"id":"a","session":"tab","result":{"exit_code":0}}),
        );
        assert_eq!(s.request("GET", "/jobs/a", json!({})).1["status"], "done");
    }
    #[test]
    fn pause_cancels_undelivered_command() {
        let mut s = State::default();
        actor(&mut s);
        s.request("POST", "/sessions/tab/claim", json!({"owner":"one"}));
        s.request(
            "POST",
            "/sessions/tab/exec",
            json!({"owner":"one","request_id":"a","command":"pwd"}),
        );
        s.request("POST", "/pause", json!({"paused":true}));
        actor(&mut s);
        assert_eq!(
            s.request("GET", "/jobs/a", json!({})).1["status"],
            "cancelled"
        );
    }
    #[test]
    fn shutdown_cancels_delivery_without_changing_pause_preference() {
        for paused in [false, true] {
            let mut state = State::default();
            actor(&mut state);
            state.request("POST", "/sessions/tab/claim", json!({"owner":"one"}));
            state.request(
                "POST",
                "/sessions/tab/exec",
                json!({"owner":"one","request_id":"shutdown","command":"pwd"}),
            );
            state.paused = paused;
            state.request("POST", "/shutdown", json!({}));
            assert_eq!(state.paused, paused);
            assert!(state.shutdown);
            assert_eq!(
                state.request("GET", "/jobs/shutdown", json!({})).1["status"],
                "cancelled"
            );
            assert_eq!(
                state
                    .request("POST", "/actor/poll", json!({"id":"tab","connected":true}))
                    .0,
                503
            );
        }
    }
    #[test]
    fn pause_and_resume_preferences_are_saved_before_exit() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-fixtures")
            .join(crate::id());
        let path = root.join("preferences.json");
        let mut state = State::default();
        state.persist_preferences_at(path.clone());
        for paused in [true, false] {
            assert_eq!(
                state.request("POST", "/pause", json!({"paused":paused})).0,
                200
            );
            assert_eq!(crate::load_json::<Value>(&path).unwrap()["paused"], paused);
        }
        state.request("POST", "/shutdown", json!({}));
        assert!(!state.paused);
    }
    #[test]
    fn unknown_result_holds_session_until_acknowledged() {
        let mut s = State::default();
        actor(&mut s);
        s.request("POST", "/sessions/tab/claim", json!({"owner":"one"}));
        s.request(
            "POST",
            "/sessions/tab/exec",
            json!({"owner":"one","request_id":"a","command":"slow"}),
        );
        actor(&mut s);
        s.request(
            "POST",
            "/actor/result",
            json!({"id":"a","session":"tab","result":{"outcome":"unknown"}}),
        );
        assert_eq!(
            s.request("POST", "/sessions/tab/release", json!({"owner":"one"}))
                .0,
            409
        );
        assert_eq!(
            s.request("POST", "/sessions/tab/acknowledge", json!({"owner":"two"}))
                .0,
            409
        );
        assert_eq!(
            s.request("POST", "/sessions/tab/acknowledge", json!({"owner":"one"}))
                .0,
            200
        );
        assert_eq!(
            s.request("POST", "/sessions/tab/release", json!({"owner":"one"}))
                .0,
            200
        );
    }
    #[test]
    fn authenticated_loopback_round_trip() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let d = Discovery {
            version: crate::VERSION.into(),
            port,
            token: crate::id(),
            epoch: crate::id(),
            pid: std::process::id(),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker_d = d.clone();
        let worker = std::thread::spawn(move || {
            serve(
                listener,
                worker_d,
                Arc::new(Mutex::new(State::default())),
                worker_stop,
            )
        });
        let good = http::call(&d, "GET", "/health", &json!({}));
        let mut bad = d.clone();
        bad.token = "wrong".into();
        let rejected = http::call(&bad, "GET", "/health", &json!({}));
        stop.store(true, Ordering::Relaxed);
        worker.join().unwrap().unwrap();
        assert_eq!(good.unwrap()["ok"], true);
        assert_eq!(rejected.unwrap()["http_status"], 401);
    }

    fn unresolved() -> State {
        let mut s = State::default();
        actor(&mut s);
        s.request("POST", "/sessions/tab/claim", json!({"owner":"one"}));
        s.request(
            "POST",
            "/sessions/tab/exec",
            json!({"owner":"one","request_id":"original","command":"pwd"}),
        );
        actor(&mut s);
        s.request(
            "POST",
            "/actor/result",
            json!({"id":"original","session":"tab","result":{"outcome":"unknown"}}),
        );
        s
    }
    fn recovery() -> Value {
        json!({"owner":"one","request_id":"recovery","job_id":"original"})
    }
    #[test]
    fn recovery_sends_only_ctrl_c_once_and_retains_original_hold() {
        let mut s = unresolved();
        let mut p = recovery();
        p["text"] = "ignored arbitrary input".into();
        assert_eq!(
            s.request("POST", "/sessions/tab/interrupt", p.clone()).0,
            200
        );
        assert_eq!(
            s.request("POST", "/sessions/tab/acknowledge", json!({"owner":"one"}))
                .0,
            409
        );
        let poll = s
            .request("POST", "/actor/poll", json!({"id":"tab","connected":true}))
            .1;
        assert_eq!(poll["job"]["id"], "recovery");
        assert_eq!(poll["job"]["payload"]["text"], "\u{3}");
        assert_eq!(
            s.request("POST", "/sessions/tab/interrupt", p).1["duplicate"],
            true
        );
        assert!(s
            .request("POST", "/actor/poll", json!({"id":"tab","connected":true}))
            .1["job"]
            .is_null());
        assert_eq!(
            s.request("POST", "/sessions/tab/acknowledge", json!({"owner":"one"}))
                .0,
            409
        );
        s.request(
            "POST",
            "/actor/result",
            json!({"id":"recovery","session":"tab","result":{"ok":true,"sent":true}}),
        );
        assert_eq!(s.sessions["tab"].busy.as_deref(), Some("original"));
        assert_eq!(s.jobs["original"].status, "unknown");
        assert_eq!(
            s.request(
                "POST",
                "/sessions/tab/input",
                json!({"owner":"one","request_id":"extra","text":"pwd\r"})
            )
            .0,
            409
        );
        assert_eq!(
            s.request("POST", "/sessions/tab/claim", json!({"owner":"two"}))
                .0,
            409
        );
        actor(&mut s);
        assert_eq!(
            s.request("POST", "/sessions/tab/acknowledge", json!({"owner":"one"}))
                .0,
            200
        );
        assert_eq!(
            s.request(
                "POST",
                "/sessions/tab/exec",
                json!({"owner":"one","request_id":"next","command":"pwd"})
            )
            .0,
            200
        );
    }
    #[test]
    fn recovery_rejects_wrong_owner_wrong_job_running_paused_and_expired_lease() {
        for case in [
            "owner",
            "job",
            "running",
            "paused",
            "session_paused",
            "lease",
            "offline",
        ] {
            let mut s = unresolved();
            let mut p = recovery();
            match case {
                "owner" => p["owner"] = "two".into(),
                "job" => p["job_id"] = "other-job".into(),
                "running" => s.jobs.get_mut("original").unwrap().status = "running".into(),
                "paused" => s.paused = true,
                "session_paused" => s.sessions.get_mut("tab").unwrap().paused = true,
                "lease" => s.sessions.get_mut("tab").unwrap().lease_until = 0,
                "offline" => s.sessions.get_mut("tab").unwrap().connected = false,
                _ => unreachable!(),
            }
            assert_eq!(
                s.request("POST", "/sessions/tab/interrupt", p).0,
                409,
                "{case}"
            );
            assert!(s.sessions["tab"].interrupt.is_none());
        }
    }
    #[test]
    fn late_original_result_cancels_pending_recovery() {
        let mut s = unresolved();
        s.request("POST", "/sessions/tab/interrupt", recovery());
        s.request(
            "POST",
            "/actor/result",
            json!({"id":"original","session":"tab","result":{"outcome":"completed","exit_code":0}}),
        );
        assert_eq!(s.jobs["recovery"].status, "cancelled");
        assert!(s
            .request("POST", "/actor/poll", json!({"id":"tab","connected":true}))
            .1["job"]
            .is_null());
    }
    #[test]
    fn recovery_pause_prevents_delivery_and_unknown_recovery_can_be_acknowledged() {
        for global in [false, true] {
            let mut s = unresolved();
            s.request("POST", "/sessions/tab/interrupt", recovery());
            s.request(
                "POST",
                if global {
                    "/pause"
                } else {
                    "/sessions/tab/pause"
                },
                json!({"paused":true}),
            );
            assert_eq!(s.jobs["recovery"].status, "cancelled");
            assert!(s
                .request("POST", "/actor/poll", json!({"id":"tab","connected":true}))
                .1["job"]
                .is_null());
        }
        let mut s = unresolved();
        s.request("POST", "/sessions/tab/interrupt", recovery());
        actor(&mut s);
        s.request(
            "POST",
            "/actor/result",
            json!({"id":"recovery","session":"tab","result":{"outcome":"unknown"}}),
        );
        assert_eq!(
            s.request(
                "POST",
                "/sessions/tab/interrupt",
                json!({"owner":"one","request_id":"retry","job_id":"original"})
            )
            .0,
            409
        );
        assert_eq!(
            s.request("POST", "/sessions/tab/acknowledge", json!({"owner":"one"}))
                .0,
            200
        );
        assert_eq!(s.jobs["recovery"].status, "acknowledged");
        assert_eq!(s.jobs["original"].status, "acknowledged");
    }
}
