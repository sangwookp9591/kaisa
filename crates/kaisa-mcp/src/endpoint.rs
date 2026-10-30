//! The attempt's broker endpoint: a Unix socket private to the user, served by a process outside
//! the agent's sandbox (architecture §4, §5.6).
//!
//! The socket sits in the attempt directory when that path fits the OS limit on socket paths;
//! otherwise in a private per-user runtime directory under the temporary directory. Either way
//! the attempt directory records where it is, in `broker.endpoint`.
//!
//! A connection must first send one line, `{"token": "<capability>"}`. Only a caller holding the
//! attempt's token is served; anyone else gets one refusal line and the connection is closed.
//! After the hello the connection carries MCP exactly as the stdio bridge does.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::attempt::{BridgeConfig, attempt_server};
use crate::token::AttemptToken;

/// The socket file name when it fits in the attempt directory.
pub const BROKER_SOCKET: &str = "broker.sock";

/// The file in an attempt directory recording where its broker socket is.
pub const BROKER_ENDPOINT_FILE: &str = "broker.endpoint";

/// Longest socket path used, in bytes: below the smallest OS limit (104 on macOS).
const SOCKET_PATH_LIMIT: usize = 100;

/// Where the attempt's socket goes, creating a private runtime directory if the attempt
/// directory's path is too long.
fn socket_path(attempt_directory: &Path) -> io::Result<PathBuf> {
    let beside: PathBuf = attempt_directory.join(BROKER_SOCKET);
    if beside.as_os_str().len() < SOCKET_PATH_LIMIT {
        return Ok(beside);
    }
    let own_uid: u32 = kaisa_sys::current_uid();
    let runtime: PathBuf = std::env::temp_dir().join(format!("kaisa-{own_uid}"));
    kaisa_sys::ensure_private_directory(&runtime)?;
    let metadata = std::fs::symlink_metadata(&runtime)?;
    if !metadata.is_dir() || std::os::unix::fs::MetadataExt::uid(&metadata) != own_uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} is not a private directory of this user",
                runtime.display()
            ),
        ));
    }
    let name: String = blake3::hash(attempt_directory.as_os_str().as_encoded_bytes()).to_hex()
        [..SOCKET_NAME_DIGITS]
        .to_owned();
    Ok(runtime.join(format!("{name}.sock")))
}

/// Hex digits of the attempt-path digest naming a runtime socket.
const SOCKET_NAME_DIGITS: usize = 24;

/// The socket of the attempt in `attempt_directory`, as its broker recorded it.
fn recorded_socket(attempt_directory: &Path) -> io::Result<PathBuf> {
    Ok(PathBuf::from(
        std::fs::read_to_string(attempt_directory.join(BROKER_ENDPOINT_FILE))?.trim(),
    ))
}

/// The longest hello line accepted, in bytes; a token line is far shorter.
const HELLO_LIMIT: u64 = 1024;

/// The attempt's broker, bound and ready to accept bridges.
#[derive(Debug)]
pub struct BrokerEndpoint {
    listener: UnixListener,
    config: BridgeConfig,
    token: AttemptToken,
    path: PathBuf,
}

impl BrokerEndpoint {
    /// Binds the broker socket of the attempt in `attempt_directory`, whose bridge config and
    /// token must already be written, and records where it is.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the config or token cannot be read or the socket cannot be bound.
    pub fn bind(attempt_directory: &Path) -> io::Result<Self> {
        let path: PathBuf = socket_path(attempt_directory)?;
        let _ = std::fs::remove_file(&path);
        let listener: UnixListener = UnixListener::bind(&path)?;
        std::fs::write(
            attempt_directory.join(BROKER_ENDPOINT_FILE),
            path.as_os_str().as_encoded_bytes(),
        )?;
        Ok(Self {
            listener,
            config: BridgeConfig::load(attempt_directory)?,
            token: AttemptToken::load(attempt_directory)?,
            path,
        })
    }

    /// Accepts and serves one bridge connection to its end.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if accepting or serving fails. A refused caller is not an error.
    pub fn serve_one(&self) -> io::Result<()> {
        let (stream, _) = self.listener.accept()?;
        let own_uid: u32 = kaisa_sys::current_uid();
        if kaisa_sys::require_same_user(kaisa_sys::peer_uid(&stream)?, own_uid).is_err() {
            return Ok(());
        }
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut hello = String::new();
        io::Read::take(&mut reader, HELLO_LIMIT).read_line(&mut hello)?;
        let presented: String = serde_json::from_str::<Value>(&hello)
            .ok()
            .and_then(|value| value["token"].as_str().map(str::to_owned))
            .unwrap_or_default();
        let mut writer: UnixStream = stream;
        if !self.token.admits(&presented) {
            // Best effort: a refused caller may already have hung up.
            let _ = writeln!(
                writer,
                "{}",
                json!({"error": "this broker serves another attempt"})
            );
            return Ok(());
        }
        attempt_server(&self.config)?
            .serve(reader, &mut writer)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("{error:?}")))
    }
}

/// Relays an agent's MCP stream to its attempt's broker and back, performing no effects itself:
/// what `kaisa mcp --attempt` does when the broker runs outside the agent's sandbox.
///
/// # Errors
///
/// Returns the I/O error if the token cannot be read, the broker cannot be reached, or the
/// stream breaks.
pub fn relay(
    attempt_directory: &Path,
    mut input: impl io::Read + Send + 'static,
    output: &mut dyn Write,
) -> io::Result<()> {
    let token: AttemptToken = AttemptToken::load(attempt_directory)?;
    let mut stream: UnixStream = UnixStream::connect(recorded_socket(attempt_directory)?)?;
    writeln!(stream, "{}", json!({"token": token.as_str()}))?;
    let mut upstream: UnixStream = stream.try_clone()?;
    let forward = std::thread::spawn(move || -> io::Result<()> {
        io::copy(&mut input, &mut upstream)?;
        upstream.shutdown(std::net::Shutdown::Write)
    });
    io::copy(&mut stream, output)?;
    forward
        .join()
        .map_err(|_| io::Error::other("the relay's forwarding thread panicked"))?
}

impl BrokerEndpoint {
    /// Where the socket is bound.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.path
    }
}

impl Drop for BrokerEndpoint {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};

    use kaisa_domain::{RepoPath, Role, WriteSet};
    use serde_json::json;

    use super::{BROKER_ENDPOINT_FILE, BROKER_SOCKET, BrokerEndpoint, recorded_socket, relay};
    use crate::attempt::BridgeConfig;
    use crate::token::AttemptToken;

    /// A token no attempt was ever issued.
    const FORGED_TOKEN: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    /// An attempt owning `src/auth`, with config and token written, removed on drop.
    struct Attempt {
        base: PathBuf,
        token: AttemptToken,
    }

    impl Attempt {
        fn new(name: &str) -> Self {
            let base: PathBuf =
                std::env::temp_dir().join(format!("kb-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            for directory in ["w/src/auth", "a"] {
                std::fs::create_dir_all(base.join(directory)).expect("writable temp dir");
            }
            BridgeConfig {
                worktree: base.join("w"),
                write_set: WriteSet::new(vec![RepoPath::try_new("src/auth").expect("valid")]),
                role: Role::Developer,
                scratch: None,
                broker_port: None,
            }
            .write_to(&base.join("a"))
            .expect("written");
            let token = AttemptToken::issue(&base.join("a")).expect("issued");
            Self { base, token }
        }

        fn directory(&self) -> PathBuf {
            self.base.join("a")
        }
    }

    impl Drop for Attempt {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    /// Connects, says hello with `token`, writes one file, and returns the first reply line.
    fn a_bridge_writes(socket: &Path, token: &str) -> String {
        let mut stream = UnixStream::connect(socket).expect("the broker is listening");
        let requests = [
            json!({"token": token}),
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                   "params": {"name": "fs_write", "arguments": {"path": "src/auth/notes.md", "content": "notes\n"}}}),
        ];
        for request in requests {
            writeln!(stream, "{request}").expect("sent");
        }
        stream
            .shutdown(std::net::Shutdown::Write)
            .expect("closed for writing");
        let mut reply = String::new();
        BufReader::new(stream)
            .read_line(&mut reply)
            .expect("a reply");
        reply
    }

    fn serve_one_in_background(attempt: &Attempt) -> std::thread::JoinHandle<()> {
        let endpoint = BrokerEndpoint::bind(&attempt.directory()).expect("bound");
        std::thread::spawn(move || endpoint.serve_one().expect("served"))
    }

    #[test]
    fn a_bridge_with_the_right_token_is_served() {
        let attempt = Attempt::new("ok");
        let broker = serve_one_in_background(&attempt);

        let reply: String = a_bridge_writes(
            &attempt.directory().join(BROKER_SOCKET),
            attempt.token.as_str(),
        );
        broker.join().expect("the broker thread finishes");

        assert!(
            reply.contains("\"isError\":false")
                && attempt.base.join("w/src/auth/notes.md").exists(),
            "the attempt's own bridge acts through the broker; got {reply}"
        );
    }

    #[test]
    fn a_bridge_with_a_forged_token_changes_nothing() {
        let attempt = Attempt::new("forged");
        let broker = serve_one_in_background(&attempt);

        let reply: String = a_bridge_writes(
            &recorded_socket(&attempt.directory()).expect("recorded"),
            FORGED_TOKEN,
        );
        broker.join().expect("the broker thread finishes");

        assert!(
            reply.contains("another attempt") && !attempt.base.join("w/src/auth/notes.md").exists(),
            "a caller without the capability is refused before any tool runs; got {reply}"
        );
    }

    #[test]
    fn the_relay_performs_no_file_writes_itself() {
        let attempt = Attempt::new("relay");
        let socket: PathBuf = attempt.directory().join(BROKER_SOCKET);
        let listener =
            std::os::unix::net::UnixListener::bind(&socket).expect("a stand-in broker binds");
        std::fs::write(
            attempt.directory().join(BROKER_ENDPOINT_FILE),
            socket.as_os_str().as_encoded_bytes(),
        )
        .expect("the stand-in records its socket");
        let stand_in = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("the relay connects");
            let received: Vec<String> = BufReader::new(stream.try_clone().expect("cloned"))
                .lines()
                .map(|line| line.expect("readable"))
                .collect();
            writeln!(
                &stream,
                "{}",
                json!({"jsonrpc": "2.0", "id": 1, "result": {}})
            )
            .expect("sent");
            received
        });
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "fs_write", "arguments": {"path": "src/auth/notes.md", "content": "x"}}});
        let mut agent_sees: Vec<u8> = Vec::new();

        relay(
            &attempt.directory(),
            std::io::Cursor::new(format!("{request}\n")),
            &mut agent_sees,
        )
        .expect("the relay runs");
        let broker_received: Vec<String> = stand_in.join().expect("the stand-in finishes");

        assert_eq!(
            broker_received.len(),
            2,
            "the broker receives the token hello and the agent's request, nothing else"
        );
        assert!(
            broker_received[1].contains("fs_write")
                && !attempt.base.join("w/src/auth/notes.md").exists(),
            "the write request was forwarded, and the relay itself changed no file"
        );
        assert!(
            String::from_utf8_lossy(&agent_sees).contains("\"result\""),
            "the broker's reply reaches the agent"
        );
    }

    #[test]
    fn an_attempt_directory_too_deep_for_a_socket_path_still_gets_a_private_broker() {
        let attempt = Attempt::new("deep");
        let deep: PathBuf = attempt
            .directory()
            .join("x".repeat(super::SOCKET_PATH_LIMIT));
        std::fs::create_dir_all(&deep).expect("writable");
        for file in [crate::attempt::BRIDGE_CONFIG_FILE, crate::token::TOKEN_FILE] {
            std::fs::copy(attempt.directory().join(file), deep.join(file)).expect("copied");
        }

        let endpoint = BrokerEndpoint::bind(&deep).expect("bound despite the long path");
        let socket: PathBuf = endpoint.socket().to_owned();

        assert!(
            socket.as_os_str().len() < super::SOCKET_PATH_LIMIT
                && recorded_socket(&deep).expect("recorded") == socket,
            "the socket moved to a short private path and the attempt records it: {}",
            socket.display()
        );
    }
}
