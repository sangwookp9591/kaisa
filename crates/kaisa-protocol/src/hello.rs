//! The opening handshake: both sides state their protocol version and capabilities.
//!
//! Architecture §6: there is exactly one protocol version. Additions are negotiated as
//! capabilities, and an operation may only be used if both sides advertised it.

use std::fmt;

use kaisa_domain::Authority;
use serde::{Deserialize, Serialize};

use crate::PROTOCOL_VERSION;

/// Which endpoint a connection arrived on. This, and nothing the client sends, decides the
/// authority of everything on the connection (architecture §5.7, P10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listener {
    /// The control endpoint: reachable only by the daemon's own user (T14.3, T14.4).
    Control,
    /// A requester bridge: an agent session the user works in, such as `/kaisa`.
    Requester,
    /// A per-attempt bridge an agent was started with.
    AgentBridge,
}

impl Listener {
    /// The authority every request on this endpoint carries.
    #[must_use]
    pub fn authority(self) -> Authority {
        match self {
            Self::Control => Authority::Human,
            Self::Requester => Authority::Requester,
            Self::AgentBridge => Authority::Agent,
        }
    }
}

/// A named feature of the protocol, such as `events.stream`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Capability(String);

impl Capability {
    /// Names a capability.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self(name.to_owned())
    }

    /// The capability's name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The first frame each side sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// The protocol version the sender speaks.
    pub protocol_version: u32,
    /// Who is speaking, for diagnostics (for example `kaisa-cli 0.1.0`).
    pub peer: String,
    /// Every capability the sender supports.
    pub capabilities: Vec<Capability>,
}

impl Hello {
    /// A hello for this build's protocol version.
    #[must_use]
    pub fn current(peer: &str, capabilities: Vec<Capability>) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            peer: peer.to_owned(),
            capabilities,
        }
    }
}

/// The agreed terms of a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    capabilities: Vec<Capability>,
    authority: Authority,
}

impl Session {
    /// The authority of this connection, fixed by the endpoint it arrived on.
    #[must_use]
    pub fn authority(&self) -> Authority {
        self.authority
    }

    /// True when both sides advertised `capability`.
    #[must_use]
    pub fn supports(&self, capability: &Capability) -> bool {
        self.capabilities.contains(capability)
    }

    /// The capabilities both sides share, in our advertised order.
    #[must_use]
    pub fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }
}

/// Why two peers cannot talk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HandshakeError {
    /// The peers speak different protocol versions.
    VersionMismatch {
        /// Our version.
        ours: u32,
        /// The peer's version.
        theirs: u32,
    },
}

impl fmt::Display for HandshakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VersionMismatch { ours, theirs } => write!(
                f,
                "peer speaks protocol version {theirs}, this build speaks {ours}; upgrade the older one"
            ),
        }
    }
}

impl std::error::Error for HandshakeError {}

/// Agrees on the terms of a connection from our hello and the peer's.
///
/// # Errors
///
/// Returns [`HandshakeError::VersionMismatch`] if the protocol versions differ.
pub fn negotiate(
    ours: &Hello,
    theirs: &Hello,
    listener: Listener,
) -> Result<Session, HandshakeError> {
    if ours.protocol_version != theirs.protocol_version {
        return Err(HandshakeError::VersionMismatch {
            ours: ours.protocol_version,
            theirs: theirs.protocol_version,
        });
    }
    let capabilities: Vec<Capability> = ours
        .capabilities
        .iter()
        .filter(|capability| theirs.capabilities.contains(capability))
        .cloned()
        .collect();
    Ok(Session {
        capabilities,
        authority: listener.authority(),
    })
}

#[cfg(test)]
mod tests {
    use kaisa_domain::Authority;

    use super::{Capability, HandshakeError, Hello, Listener, Session, negotiate};
    use crate::frame::test_support::TrickleStream;
    use crate::{DEFAULT_FRAME_LIMIT, FrameReader, PROTOCOL_VERSION, write_frame};

    /// Bytes per read when the hello is read back.
    const TRICKLE_CHUNK: usize = 16;

    fn events_stream() -> Capability {
        Capability::new("events.stream")
    }

    fn steer_attempts() -> Capability {
        Capability::new("attempts.steer")
    }

    fn effect_previews() -> Capability {
        Capability::new("effects.preview")
    }

    /// The daemon of this build: streams events, steers attempts, previews effects.
    fn daemon_hello() -> Hello {
        Hello::current(
            "kaisad",
            vec![events_stream(), steer_attempts(), effect_previews()],
        )
    }

    /// An older CLI that predates effect previews but knows about a capability the daemon
    /// dropped.
    fn older_cli_hello() -> Hello {
        Hello::current(
            "kaisa-cli",
            vec![
                events_stream(),
                steer_attempts(),
                Capability::new("legacy.poll"),
            ],
        )
    }

    /// A client from a different protocol generation.
    fn client_from_another_protocol_version() -> Hello {
        Hello {
            protocol_version: PROTOCOL_VERSION + 1,
            ..older_cli_hello()
        }
    }

    #[test]
    fn a_written_hello_is_read_back_unchanged() {
        let hello: Hello = daemon_hello();
        let mut wire: Vec<u8> = Vec::new();
        write_frame(&mut wire, &hello).expect("writing to memory succeeds");
        let mut reader =
            FrameReader::new(TrickleStream::new(wire, TRICKLE_CHUNK), DEFAULT_FRAME_LIMIT);

        let read: Option<Hello> = reader.read_frame().expect("a hello frame reads");

        assert_eq!(
            read,
            Some(hello),
            "the handshake survives the wire unchanged"
        );
    }

    #[test]
    fn negotiation_keeps_only_capabilities_both_sides_advertise() {
        let session: Session = negotiate(&daemon_hello(), &older_cli_hello(), Listener::Control)
            .expect("the versions match");

        assert_eq!(
            session.capabilities(),
            [events_stream(), steer_attempts()],
            "a capability is usable only when both sides know it"
        );
    }

    #[test]
    fn a_capability_only_one_side_offered_is_not_supported() {
        let session: Session = negotiate(&daemon_hello(), &older_cli_hello(), Listener::Control)
            .expect("the versions match");

        assert!(
            !session.supports(&effect_previews()),
            "the daemon must not send previews to a client that cannot read them"
        );
    }

    #[test]
    fn peers_with_no_shared_capabilities_still_agree_on_an_empty_session() {
        let bare_client: Hello = Hello::current("bare-client", Vec::new());

        let session: Session = negotiate(&daemon_hello(), &bare_client, Listener::Control)
            .expect("the versions match");

        assert!(
            session.capabilities().is_empty(),
            "basic requests still work; nothing optional is assumed"
        );
    }

    #[test]
    fn a_peer_with_a_different_protocol_version_is_refused() {
        let ours: Hello = daemon_hello();
        let theirs: Hello = client_from_another_protocol_version();

        assert_eq!(
            negotiate(&ours, &theirs, Listener::Control),
            Err(HandshakeError::VersionMismatch {
                ours: ours.protocol_version,
                theirs: theirs.protocol_version
            }),
            "there is one protocol version; mismatches are refused, never guessed"
        );
    }

    /// A client that claims more than its endpoint gives, as a compromised agent might.
    fn an_agent_claiming_human_authority() -> Hello {
        Hello::current(
            "claude-code (attempt att_41a)",
            vec![Capability::new("authority.human")],
        )
    }

    #[test]
    fn a_hello_on_the_control_endpoint_gets_human_authority() {
        let session: Session = negotiate(&daemon_hello(), &older_cli_hello(), Listener::Control)
            .expect("the versions match");

        assert_eq!(
            session.authority(),
            Authority::Human,
            "the user's own CLI on the control endpoint speaks for the user"
        );
    }

    #[test]
    fn an_agent_bridge_cannot_claim_human_authority() {
        let session: Session = negotiate(
            &daemon_hello(),
            &an_agent_claiming_human_authority(),
            Listener::AgentBridge,
        )
        .expect("the versions match");

        assert_eq!(
            session.authority(),
            Authority::Agent,
            "authority comes from where a connection arrived, never from what it claims"
        );
    }
}
