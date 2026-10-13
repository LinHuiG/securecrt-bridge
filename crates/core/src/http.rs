//! Small bounded HTTP/1.1 transport. Loopback only; one JSON request per socket.
use crate::Discovery;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddrV4, TcpStream},
    time::Duration,
};
pub const MAX_BODY: usize = 2 * 1024 * 1024;

#[derive(Debug)]
pub struct Message {
    pub first: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}
pub fn read(stream: &mut impl Read) -> Result<Message> {
    let mut all = Vec::new();
    let mut byte = [0];
    while !all.ends_with(b"\r\n\r\n") {
        if all.len() >= 16 * 1024 {
            bail!("HTTP headers too large");
        }
        stream.read_exact(&mut byte)?;
        all.push(byte[0]);
    }
    let head = std::str::from_utf8(&all)?;
    let mut lines = head.split("\r\n");
    let first = lines.next().context("HTTP first line missing")?.to_owned();
    let mut headers = BTreeMap::new();
    for line in lines.filter(|s| !s.is_empty()) {
        let (key, value) = line.split_once(':').context("Invalid HTTP header")?;
        let key = key.trim().to_ascii_lowercase();
        if headers.insert(key, value.trim().to_owned()).is_some() {
            bail!("Duplicate HTTP header");
        }
    }
    if headers.contains_key("transfer-encoding") {
        bail!("Chunked transfer is not supported");
    }
    let size: usize = headers
        .get("content-length")
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(0);
    if size > MAX_BODY {
        bail!("HTTP body too large");
    }
    let mut body = vec![0; size];
    stream.read_exact(&mut body)?;
    Ok(Message {
        first,
        headers,
        body,
    })
}
pub fn respond(stream: &mut TcpStream, status: u16, value: &Value) -> Result<()> {
    let body = serde_json::to_vec(value)?;
    write!(stream,"HTTP/1.1 {} Response\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",status,body.len())?;
    stream.write_all(&body)?;
    Ok(())
}
pub fn call(cfg: &Discovery, method: &str, path: &str, data: &Value) -> Result<Value> {
    if !matches!(method, "GET" | "POST")
        || !path.starts_with('/')
        || path.contains(['\r', '\n', ' '])
    {
        bail!("Invalid local request");
    }
    let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, cfg.port);
    let mut stream = TcpStream::connect_timeout(&addr.into(), Duration::from_millis(500))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let body = if method == "GET" {
        Vec::new()
    } else {
        serde_json::to_vec(data)?
    };
    write!(stream,"{} {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",method,path,cfg.port,cfg.token,body.len())?;
    stream.write_all(&body)?;
    let response = read(&mut stream)?;
    let status: u16 = response
        .first
        .split_whitespace()
        .nth(1)
        .context("Bad HTTP response")?
        .parse()?;
    let mut result: Value = serde_json::from_slice(&response.body)?;
    if status >= 400 {
        result["http_status"] = status.into();
    }
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_ambiguous_framing() {
        assert!(read(
            &mut &b"POST / HTTP/1.1\r\nContent-Length: 0\r\nContent-Length: 1\r\n\r\nx"[..]
        )
        .is_err());
        assert!(read(&mut &b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n"[..]).is_err());
    }
    #[test]
    fn parses_unicode_body() {
        let body = "中文".as_bytes();
        let wire = format!(
            "POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n中文",
            body.len()
        );
        assert_eq!(read(&mut wire.as_bytes()).unwrap().body, body);
    }
}
