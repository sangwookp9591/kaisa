//! The attempt's broker endpoint on Windows: two named pipes, one per direction, created by a
//! process outside the agent's AppContainer and admitting that container (T15.5b2).
//!
//! One pipe per direction because a synchronous pipe handle serializes reads and writes: a
//! relay blocked reading replies could otherwise never send the next request. The protocol is
//! the Unix one: the first line up is `{"token": "<capability>"}`, then MCP.

use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use kaisa_sys::ControlPipe;
use serde_json::{Value, json};

use crate::attempt::{BridgeConfig, attempt_server};
use crate::token::AttemptToken;

/// The file in an attempt directory recording the broker's pipe name.
pub const BROKER_ENDPOINT_FILE: &str = "broker.endpoint";

/// The file in an attempt directory naming the AppContainer SID the broker admits, if any.
pub const CONTAINER_SID_FILE: &str = "container.sid";

/// The longest hello line accepted, in bytes; a token line is far shorter.
const HELLO_LIMIT: u64 = 1024;

/// Hex digits of the attempt-path digest naming its pipes.
const PIPE_NAME_DIGITS: usize = 24;

fn pipe_names(base: &str) -> (String, String) {
    (format!("{base}-up"), format!("{base}-down"))
}

/// The attempt's broker, with both pipes created and waiting for its relay.
#[derive(Debug)]
pub struct BrokerEndpoint {
    up: ControlPipe,
    down: ControlPipe,
    config: BridgeConfig,
    token: AttemptToken,
    base: String,
    container_sid: Option<String>,
}

impl BrokerEndpoint {
    /// Creates the broker pipes of the attempt in `attempt_directory` (whose bridge config and
    /// token must exist) and records their name. If the directory names a container SID, the
    /// pipes admit that container; otherwise only this user.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the config or token cannot be read or a pipe cannot be created.
    pub fn bind(attempt_directory: &Path) -> io::Result<Self> {
        let digest = blake3::hash(attempt_directory.as_os_str().as_encoded_bytes());
        let base: String = format!(
            r"\\.\pipe\kaisa-broker-{}",
            &digest.to_hex()[..PIPE_NAME_DIGITS]
        );
        let (up_name, down_name) = pipe_names(&base);
        let sid: Option<String> =
            std::fs::read_to_string(attempt_directory.join(CONTAINER_SID_FILE))
                .ok()
                .map(|text| text.trim().to_owned());
        let create = |name: &str| match &sid {
            Some(sid) => ControlPipe::create_first_admitting(name, sid),
            None => ControlPipe::create_first(name),
        };
        let endpoint = Self {
            up: create(&up_name)?,
            down: create(&down_name)?,
            config: BridgeConfig::load(attempt_directory)?,
            token: AttemptToken::load(attempt_directory)?,
            base: base.clone(),
            container_sid: sid,
        };
        std::fs::write(attempt_directory.join(BROKER_ENDPOINT_FILE), base)?;
        Ok(endpoint)
    }

    /// The base name of the broker's pipes.
    #[must_use]
    pub fn socket(&self) -> &Path {
        Path::new(&self.base)
    }

    /// Accepts the attempt's relay and serves it to its end.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if accepting or serving fails. A refused caller is not an error.
    pub fn serve_one(self) -> io::Result<()> {
        // A caller that connects and leaves at once (an agent that never used its tools, or
        // Kaisa releasing an idle broker) is not an error: there was nothing to serve.
        for pipe in [&self.up, &self.down] {
            match pipe.accept() {
                Ok(()) => {}
                Err(error) if caller_left(&error) => return Ok(()),
                Err(error) => return Err(error),
            }
        }
        match self.up.client_is_same_user() {
            Ok(true) => {}
            Ok(false) => return Ok(()),
            Err(error) if caller_left(&error) => return Ok(()),
            Err(error) => return Err(error),
        }
        let mut reader = BufReader::new(self.up.into_stream());
        let mut writer = self.down.into_stream();
        let mut hello = String::new();
        match io::Read::take(&mut reader, HELLO_LIMIT).read_line(&mut hello) {
            Ok(_) => {}
            Err(error) if caller_left(&error) => return Ok(()),
            Err(error) => return Err(error),
        }
        let presented: String = serde_json::from_str::<Value>(&hello)
            .ok()
            .and_then(|value| value["token"].as_str().map(str::to_owned))
            .unwrap_or_default();
        if !self.token.admits(&presented) {
            // Best effort: a refused caller may already have hung up.
            let _ = writeln!(
                writer,
                "{}",
                json!({"error": "this broker serves another attempt"})
            );
            return Ok(());
        }
        attempt_server(&self.config, self.container_sid.as_deref())?
            .serve(reader, &mut writer)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("{error:?}")))
    }
}

/// `ERROR_BROKEN_PIPE`, `ERROR_NO_DATA`, `ERROR_PIPE_NOT_CONNECTED`: the client end is gone.
const CALLER_LEFT_CODES: [i32; 3] = [109, 232, 233];

/// True if `error` means the caller closed its end before or while being served.
fn caller_left(error: &io::Error) -> bool {
    error
        .raw_os_error()
        .is_some_and(|code| CALLER_LEFT_CODES.contains(&code))
}

/// Relays an agent's MCP stream to its attempt's broker and back, performing no effects itself.
///
/// # Errors
///
/// Returns the I/O error if the token or endpoint cannot be read, the broker cannot be
/// reached, or a stream breaks.
pub fn relay(
    attempt_directory: &Path,
    mut input: impl io::Read + Send + 'static,
    output: &mut dyn Write,
) -> io::Result<()> {
    let token: AttemptToken = AttemptToken::load(attempt_directory)?;
    let base: String = std::fs::read_to_string(attempt_directory.join(BROKER_ENDPOINT_FILE))?
        .trim()
        .to_owned();
    let (up_name, down_name) = pipe_names(&base);
    let mut up = std::fs::OpenOptions::new()
        .write(true)
        .open(PathBuf::from(up_name))?;
    let mut down = std::fs::OpenOptions::new()
        .read(true)
        .open(PathBuf::from(down_name))?;
    writeln!(up, "{}", json!({"token": token.as_str()}))?;
    let forward = std::thread::spawn(move || -> io::Result<()> {
        io::copy(&mut input, &mut up)?;
        // Dropping `up` here is the end-of-input the broker waits for.
        Ok(())
    });
    io::copy(&mut down, output)?;
    forward
        .join()
        .map_err(|_| io::Error::other("the relay's forwarding thread panicked"))?
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use kaisa_domain::{RepoPath, Role, WriteSet};
    use serde_json::json;

    use super::{BrokerEndpoint, relay};
    use crate::attempt::BridgeConfig;
    use crate::token::AttemptToken;

    /// An attempt owning `src/auth`, with config and token written, removed on drop.
    struct Attempt {
        base: PathBuf,
    }

    impl Attempt {
        fn new(name: &str) -> Self {
            let base: PathBuf =
                std::env::temp_dir().join(format!("kbw-{name}-{}", std::process::id()));
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
            AttemptToken::issue(&base.join("a")).expect("issued");
            Self { base }
        }
    }

    impl Drop for Attempt {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    #[test]
    fn a_relay_reaches_its_broker_over_the_pipes_and_the_write_lands() {
        let attempt = Attempt::new("pipes");
        let endpoint = BrokerEndpoint::bind(&attempt.base.join("a")).expect("bound");
        let broker = std::thread::spawn(move || endpoint.serve_one());
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "fs_write", "arguments": {"path": "src/auth/notes.md", "content": "notes\n"}}});
        let mut agent_sees: Vec<u8> = Vec::new();

        relay(
            &attempt.base.join("a"),
            std::io::Cursor::new(format!("{request}\n")),
            &mut agent_sees,
        )
        .expect("the relay runs");
        broker
            .join()
            .expect("the broker thread finishes")
            .expect("the broker serves");

        assert!(
            String::from_utf8_lossy(&agent_sees).contains("\"isError\":false")
                && attempt.base.join("w/src/auth/notes.md").exists(),
            "the agent's write travelled up one pipe and the answer came down the other: {}",
            String::from_utf8_lossy(&agent_sees)
        );
    }

    #[test]
    fn a_caller_that_connects_and_leaves_at_once_is_not_an_error() {
        let attempt = Attempt::new("leaves");
        let endpoint = BrokerEndpoint::bind(&attempt.base.join("a")).expect("bound");
        let base: String = endpoint.socket().to_string_lossy().into_owned();
        // Connect both pipes and close them before the broker accepts, as an idle release does.
        drop(
            std::fs::OpenOptions::new()
                .write(true)
                .open(format!("{base}-up")),
        );
        drop(
            std::fs::OpenOptions::new()
                .read(true)
                .open(format!("{base}-down")),
        );

        let served = endpoint.serve_one();

        assert!(
            served.is_ok(),
            "a caller gone before it was served leaves nothing to serve: {served:?}"
        );
    }
}
