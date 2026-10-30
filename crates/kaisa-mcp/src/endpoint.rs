//! The attempt's broker endpoint: a Unix socket in the private attempt directory, served by a
//! process outside the agent's sandbox (architecture §4, §5.6).
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

/// The socket file in an attempt directory.
pub const BROKER_SOCKET: &str = "broker.sock";

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
    /// Binds the broker socket in `attempt_directory`, whose bridge config and token must
    /// already be written.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the config or token cannot be read or the socket cannot be bound.
    pub fn bind(attempt_directory: &Path) -> io::Result<Self> {
        let path: PathBuf = attempt_directory.join(BROKER_SOCKET);
        Ok(Self {
            listener: UnixListener::bind(&path)?,
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
            writeln!(
                writer,
                "{}",
                json!({"error": "this broker serves another attempt"})
            )?;
            return Ok(());
        }
        attempt_server(&self.config)?
            .serve(reader, &mut writer)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("{error:?}")))
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

    use super::{BROKER_SOCKET, BrokerEndpoint};
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

        let reply: String = a_bridge_writes(&attempt.directory().join(BROKER_SOCKET), FORGED_TOKEN);
        broker.join().expect("the broker thread finishes");

        assert!(
            reply.contains("another attempt") && !attempt.base.join("w/src/auth/notes.md").exists(),
            "a caller without the capability is refused before any tool runs; got {reply}"
        );
    }
}
