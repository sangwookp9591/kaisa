//! Tool-less chat over HTTP to a configured loopback model server.

use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use serde_json::{Value, json};

/// Maximum bytes accepted for an HTTP response, including headers and chunk framing.
pub const MAX_RESPONSE_SIZE: usize = 8 * 1024 * 1024;
const DEFAULT_HTTP_PORT: u16 = 80;

/// A validated HTTP endpoint whose connection always goes to IPv4 loopback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalEndpoint {
    authority: String,
    port: u16,
    path: String,
}

impl LocalEndpoint {
    /// Accepts only `http://127.0.0.1[:port][/path]` or `http://localhost[:port][/path]`.
    ///
    /// An omitted port means 80. Trailing path slashes are removed before appending
    /// `/chat/completions`; queries, fragments, userinfo and request whitespace are refused.
    ///
    /// # Errors
    /// Returns [`LocalModelError::RefusedEndpoint`] for any unsupported URL.
    pub fn parse(url: &str) -> Result<Self, LocalModelError> {
        let refused = LocalModelError::RefusedEndpoint;
        if !url.is_ascii()
            || url
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
            || url.contains(['\\', '?', '#', '@'])
        {
            return Err(refused);
        }
        let remainder = url.strip_prefix("http://").ok_or(refused)?;
        let (authority, path) = remainder.split_once('/').unwrap_or((remainder, ""));
        let (host, port) = match authority.split_once(':') {
            Some((host, port)) => {
                if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(LocalModelError::RefusedEndpoint);
                }
                (
                    host,
                    port.parse::<u16>()
                        .map_err(|_| LocalModelError::RefusedEndpoint)?,
                )
            }
            None => (authority, DEFAULT_HTTP_PORT),
        };
        if !matches!(host, "127.0.0.1" | "localhost") || port == 0 {
            return Err(LocalModelError::RefusedEndpoint);
        }
        let path = path.trim_end_matches('/');
        Ok(Self {
            authority: authority.to_owned(),
            port,
            path: if path.is_empty() {
                String::new()
            } else {
                format!("/{path}")
            },
        })
    }
}

/// One text message supplied to a local model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    /// OpenAI-compatible role, such as `system`, `user` or `assistant`.
    pub role: String,
    /// The text supplied for this role.
    pub content: String,
}

/// Why a local model request could not return assistant text.
#[derive(Debug)]
pub enum LocalModelError {
    /// The URL is not a supported loopback HTTP endpoint.
    RefusedEndpoint,
    /// Connecting, writing or reading failed, including a socket timeout.
    Io(io::Error),
    /// The server returned a status other than 200.
    HttpStatus(u16),
    /// HTTP framing, JSON or the assistant content was invalid or missing.
    Malformed,
    /// The HTTP response exceeded [`MAX_RESPONSE_SIZE`].
    TooLarge,
}

impl fmt::Display for LocalModelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RefusedEndpoint => {
                formatter.write_str("only loopback HTTP model endpoints are supported")
            }
            Self::Io(error) => write!(formatter, "local model I/O failed: {error}"),
            Self::HttpStatus(status) => {
                write!(formatter, "local model returned HTTP status {status}")
            }
            Self::Malformed => formatter.write_str("local model returned a malformed response"),
            Self::TooLarge => formatter.write_str("local model response exceeded the size limit"),
        }
    }
}

impl std::error::Error for LocalModelError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for LocalModelError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Posts a non-streaming chat request and returns the first assistant choice's text.
///
/// The timeout applies to connection establishment and each socket read and write.
/// This client never resolves hostnames or follows redirects.
///
/// # Errors
/// Returns [`LocalModelError`] for socket errors, non-200 status, malformed responses,
/// or responses exceeding [`MAX_RESPONSE_SIZE`].
pub fn chat(
    endpoint: &LocalEndpoint,
    model: &str,
    messages: &[ChatMessage],
    timeout: Duration,
) -> Result<String, LocalModelError> {
    let messages: Vec<Value> = messages
        .iter()
        .map(|message| json!({"role": message.role, "content": message.content}))
        .collect();
    let body = json!({"model": model, "messages": messages, "stream": false}).to_string();
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, endpoint.port));
    let mut connection = TcpStream::connect_timeout(&address, timeout)?;
    connection.set_read_timeout(Some(timeout))?;
    connection.set_write_timeout(Some(timeout))?;
    write!(
        connection,
        "POST {}/chat/completions HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
        endpoint.path,
        endpoint.authority,
        body.len(),
        body
    )?;
    let response = read_response(&mut BufReader::new(connection))?;
    let response: Value =
        serde_json::from_slice(&response).map_err(|_| LocalModelError::Malformed)?;
    response
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(LocalModelError::Malformed)
}

fn read_line(reader: &mut impl BufRead, remaining: &mut usize) -> Result<String, LocalModelError> {
    let mut line = Vec::new();
    reader
        .take((*remaining + 1) as u64)
        .read_until(b'\n', &mut line)?;
    if line.len() > *remaining {
        return Err(LocalModelError::TooLarge);
    }
    *remaining -= line.len();
    let line = String::from_utf8(line).map_err(|_| LocalModelError::Malformed)?;
    line.strip_suffix("\r\n")
        .map(str::to_owned)
        .ok_or(LocalModelError::Malformed)
}

fn read_bytes(
    reader: &mut impl Read,
    size: usize,
    remaining: &mut usize,
) -> Result<Vec<u8>, LocalModelError> {
    if size > *remaining {
        return Err(LocalModelError::TooLarge);
    }
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            LocalModelError::Malformed
        } else {
            LocalModelError::Io(error)
        }
    })?;
    *remaining -= size;
    Ok(bytes)
}

fn read_response(reader: &mut impl BufRead) -> Result<Vec<u8>, LocalModelError> {
    let mut remaining = MAX_RESPONSE_SIZE;
    let status_line = read_line(reader, &mut remaining)?;
    let mut status_parts = status_line.split_whitespace();
    if !matches!(status_parts.next(), Some("HTTP/1.1" | "HTTP/1.0")) {
        return Err(LocalModelError::Malformed);
    }
    let status = status_parts
        .next()
        .filter(|code| code.len() == 3)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or(LocalModelError::Malformed)?;
    if status != 200 {
        return Err(LocalModelError::HttpStatus(status));
    }
    let mut length = None;
    let mut chunked = false;
    loop {
        let line = read_line(reader, &mut remaining)?;
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':').ok_or(LocalModelError::Malformed)?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            if length.is_some() {
                return Err(LocalModelError::Malformed);
            }
            length = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| LocalModelError::Malformed)?,
            );
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            if chunked || !value.eq_ignore_ascii_case("chunked") {
                return Err(LocalModelError::Malformed);
            }
            chunked = true;
        }
    }
    if chunked {
        if length.is_some() {
            return Err(LocalModelError::Malformed);
        }
        let mut body = Vec::new();
        loop {
            let line = read_line(reader, &mut remaining)?;
            let size = line.split(';').next().ok_or(LocalModelError::Malformed)?;
            let size = usize::from_str_radix(size, 16).map_err(|_| LocalModelError::Malformed)?;
            if size == 0 {
                while !read_line(reader, &mut remaining)?.is_empty() {}
                return Ok(body);
            }
            body.extend(read_bytes(reader, size, &mut remaining)?);
            if !read_line(reader, &mut remaining)?.is_empty() {
                return Err(LocalModelError::Malformed);
            }
        }
    }
    if let Some(size) = length {
        return read_bytes(reader, size, &mut remaining);
    }
    let mut body = Vec::new();
    reader.take((remaining + 1) as u64).read_to_end(&mut body)?;
    if body.len() > remaining {
        return Err(LocalModelError::TooLarge);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use serde_json::Value;

    use super::{ChatMessage, LocalEndpoint, LocalModelError, MAX_RESPONSE_SIZE, chat};

    const MODEL: &str = "local-light";
    const TIMEOUT: Duration = Duration::from_secs(5);
    const ASSISTANT_TEXT: &str = "complete café reply";
    const SERVER_ERROR_STATUS: u16 = 503;

    fn model_reply() -> String {
        serde_json::json!({"choices": [{"message": {"content": ASSISTANT_TEXT}}]}).to_string()
    }

    fn local_server(reply: Vec<u8>) -> (LocalEndpoint, JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("the model server binds loopback");
        let address = listener
            .local_addr()
            .expect("the model server has an assigned port");
        let endpoint = LocalEndpoint::parse(&format!("http://{address}/configured/v1/"))
            .expect("the configured model endpoint is loopback HTTP");
        let server = thread::spawn(move || {
            let (connection, _) = listener.accept().expect("the model client connects");
            connection
                .set_read_timeout(Some(TIMEOUT))
                .expect("the fixture bounds reads");
            connection
                .set_write_timeout(Some(TIMEOUT))
                .expect("the fixture bounds writes");
            let mut connection = BufReader::new(connection);
            let mut request = String::new();
            let mut length = None;
            loop {
                let mut line = String::new();
                connection
                    .read_line(&mut line)
                    .expect("the request header arrives");
                request.push_str(&line);
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.strip_prefix("Content-Length: ") {
                    length = Some(
                        value
                            .trim()
                            .parse::<usize>()
                            .expect("the request length is numeric"),
                    );
                }
            }
            let mut body = vec![0; length.expect("the request declares its length")];
            connection
                .read_exact(&mut body)
                .expect("the full request body arrives");
            request.push_str(&String::from_utf8(body).expect("the request body is UTF-8"));
            connection
                .get_mut()
                .write_all(&reply)
                .expect("the model reply is written");
            request
        });
        (endpoint, server)
    }

    fn attempt(endpoint: &LocalEndpoint) -> Result<String, LocalModelError> {
        chat(
            endpoint,
            MODEL,
            &[ChatMessage {
                role: "user".into(),
                content: "Review this text".into(),
            }],
            TIMEOUT,
        )
    }

    #[test]
    fn a_local_model_attempt_uses_the_configured_endpoint() {
        let body = model_reply();
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let (endpoint, server) = local_server(reply.into_bytes());
        assert_eq!(
            attempt(&endpoint).expect("the chat completes"),
            ASSISTANT_TEXT,
            "the first assistant choice supplies the result"
        );
        let request = server.join().expect("the model server finishes");
        assert!(
            request.starts_with("POST /configured/v1/chat/completions HTTP/1.1\r\n"),
            "the client uses the configured base path and chat route"
        );
        assert!(
            request.contains("\r\nContent-Type: application/json\r\nConnection: close\r\n"),
            "the client declares JSON and requests a closed connection"
        );
        let (_, body) = request
            .split_once("\r\n\r\n")
            .expect("the request has a body");
        let body: Value = serde_json::from_str(body).expect("the request contains JSON");
        assert_eq!(
            body,
            serde_json::json!({"model": MODEL, "messages": [{"role": "user", "content": "Review this text"}], "stream": false}),
            "the configured model and all messages are sent without streaming"
        );
    }

    #[test]
    fn a_chunked_reply_is_read_whole() {
        let body = model_reply();
        let mut reply = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        for byte in body.bytes() {
            reply.extend_from_slice(b"1;part=yes\r\n");
            reply.push(byte);
            reply.extend_from_slice(b"\r\n");
        }
        reply.extend_from_slice(b"0\r\nX-Model: local\r\n\r\n");
        let (endpoint, server) = local_server(reply);
        assert_eq!(
            attempt(&endpoint).expect("chunk framing is decoded"),
            ASSISTANT_TEXT,
            "every chunk including split UTF-8 bytes contributes to the assistant reply"
        );
        server.join().expect("the model server finishes");
    }

    #[test]
    fn a_remote_or_encrypted_endpoint_is_refused() {
        for url in [
            "https://localhost/v1",
            "https://models.example/v1",
            "http://models.example",
            "http://127.0.0.2",
            "http://user@localhost",
            "http://localhost:invalid",
            "http://localhost:65536",
            "http://localhost:0",
            "http://localhost/v1?query=yes",
            "http://localhost/\r\nInjected: yes",
        ] {
            assert!(
                matches!(
                    LocalEndpoint::parse(url),
                    Err(LocalModelError::RefusedEndpoint)
                ),
                "unsupported endpoint {url:?} must fail before connecting"
            );
        }
        for url in ["http://127.0.0.1", "http://localhost"] {
            assert_eq!(
                LocalEndpoint::parse(url)
                    .expect("loopback HTTP is allowed")
                    .port,
                super::DEFAULT_HTTP_PORT,
                "an omitted port uses the HTTP default"
            );
        }
    }

    #[test]
    fn a_server_error_is_reported_with_its_status() {
        let reply =
            format!("HTTP/1.1 {SERVER_ERROR_STATUS} Unavailable\r\nContent-Length: 0\r\n\r\n");
        let (endpoint, server) = local_server(reply.into_bytes());
        assert!(
            matches!(
                attempt(&endpoint),
                Err(LocalModelError::HttpStatus(SERVER_ERROR_STATUS))
            ),
            "server errors retain the HTTP status instead of becoming JSON errors"
        );
        server.join().expect("the model server finishes");
    }

    #[test]
    fn an_oversized_reply_is_refused() {
        let oversized_length = MAX_RESPONSE_SIZE + 1;
        let reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {oversized_length}\r\n\r\n");
        let (endpoint, server) = local_server(reply.into_bytes());
        assert!(
            matches!(attempt(&endpoint), Err(LocalModelError::TooLarge)),
            "an oversized declared reply is refused before allocating its body"
        );
        server.join().expect("the model server finishes");
    }
}
