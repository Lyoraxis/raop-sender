//! Minimal RTSP/1.0 client for RAOP (AirPlay 1) session setup.
//!
//! Deliberately dependency-free: std::net only. The RAOP handshake is a plain
//! text protocol; no TLS, no HTTP/2.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

#[derive(Debug)]
pub struct Response {
    pub code: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn is_ok(&self) -> bool {
        (200..300).contains(&self.code)
    }
}

pub struct RtspClient {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    cseq: u32,
    pub user_agent: String,
    /// Headers sent on every request (Client-Instance / DACP-ID / Active-Remote).
    pub identity_headers: Vec<(String, String)>,
}

impl RtspClient {
    pub fn connect(host: &str, timeout: Duration) -> std::io::Result<Self> {
        let addr = host
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, "no addr"))?;
        let stream = TcpStream::connect_timeout(&addr, timeout)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        stream.set_nodelay(true)?;
        let writer = stream.try_clone()?;
        Ok(Self {
            reader: BufReader::new(stream),
            writer,
            cseq: 0,
            user_agent: "AirPlay/550.10".to_string(),
            identity_headers: Vec::new(),
        })
    }

    /// Write a request without waiting for its response.
    pub fn send_only(
        &mut self,
        method: &str,
        uri: &str,
        headers: &[(&str, String)],
        body: &[u8],
    ) -> std::io::Result<()> {
        self.cseq += 1;
        let mut req = String::new();
        req.push_str(&format!("{method} {uri} RTSP/1.0\r\n"));
        req.push_str(&format!("CSeq: {}\r\n", self.cseq));
        req.push_str(&format!("User-Agent: {}\r\n", self.user_agent));
        for (k, v) in &self.identity_headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        if !body.is_empty() {
            req.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        req.push_str("\r\n");
        self.writer.write_all(req.as_bytes())?;
        if !body.is_empty() {
            self.writer.write_all(body)?;
        }
        self.writer.flush()
    }

    pub fn request(
        &mut self,
        method: &str,
        uri: &str,
        headers: &[(&str, String)],
        body: &[u8],
    ) -> std::io::Result<Response> {
        self.cseq += 1;
        let mut req = String::new();
        req.push_str(&format!("{method} {uri} RTSP/1.0\r\n"));
        req.push_str(&format!("CSeq: {}\r\n", self.cseq));
        req.push_str(&format!("User-Agent: {}\r\n", self.user_agent));
        for (k, v) in &self.identity_headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        if !body.is_empty() {
            req.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        req.push_str("\r\n");

        self.writer.write_all(req.as_bytes())?;
        if !body.is_empty() {
            self.writer.write_all(body)?;
        }
        self.writer.flush()?;

        self.read_response()
    }

    /// Read a response that was deferred by the receiver.
    pub fn read_pending(&mut self) -> std::io::Result<Response> {
        self.read_response()
    }

    fn read_response(&mut self) -> std::io::Result<Response> {
        let mut status = String::new();
        if self.reader.read_line(&mut status)? == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed before status line",
            ));
        }
        let mut parts = status.trim_end().splitn(3, ' ');
        let _proto = parts.next().unwrap_or("");
        let code: u16 = parts.next().and_then(|c| c.parse().ok()).unwrap_or(0);
        let reason = parts.next().unwrap_or("").to_string();

        let mut headers = Vec::new();
        loop {
            let mut line = String::new();
            if self.reader.read_line(&mut line)? == 0 {
                break;
            }
            let t = line.trim_end();
            if t.is_empty() {
                break;
            }
            if let Some((k, v)) = t.split_once(':') {
                headers.push((k.trim().to_string(), v.trim().to_string()));
            }
        }

        let len = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("Content-Length"))
            .and_then(|(_, v)| v.trim().parse::<usize>().ok())
            .unwrap_or(0);

        let mut body = vec![0u8; len];
        if len > 0 {
            self.reader.read_exact(&mut body)?;
        }

        Ok(Response {
            code,
            reason,
            headers,
            body,
        })
    }
}
