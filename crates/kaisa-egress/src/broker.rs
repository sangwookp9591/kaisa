//! The proxy itself: one listener on loopback per attempt, one thread per tunnel.

use std::io::{self, BufRead, BufReader, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::thread;

use crate::address::is_host_internal;
use crate::allow_list::AllowList;

/// The longest request head the broker reads before refusing, in bytes.
const MAX_REQUEST_HEAD: usize = 8 * 1024;

/// How the broker resolves and connects. Tests replace it; production uses [`SystemNetwork`].
pub trait Network: Send + Sync + 'static {
    /// Every address `host` resolves to.
    ///
    /// # Errors
    ///
    /// Returns the resolver's error.
    fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<IpAddr>>;

    /// Opens a connection to `address`.
    ///
    /// # Errors
    ///
    /// Returns the connection error.
    fn connect(&self, address: SocketAddr) -> io::Result<TcpStream>;
}

/// The operating system's resolver and sockets.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemNetwork;

impl Network for SystemNetwork {
    fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<IpAddr>> {
        Ok((host, port)
            .to_socket_addrs()?
            .map(|address| address.ip())
            .collect())
    }

    fn connect(&self, address: SocketAddr) -> io::Result<TcpStream> {
        TcpStream::connect(address)
    }
}

/// What the broker does with one `CONNECT host:port`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Tunnel to this address.
    Tunnel(SocketAddr),
    /// Refuse, with the reason that is logged and shown to the agent.
    Refuse(String),
}

/// Decides one `CONNECT`: the host must be allow-listed and every address it resolves to must be
/// outside the host's own networks.
#[must_use]
pub fn decide(allow_list: &AllowList, network: &dyn Network, host: &str, port: u16) -> Decision {
    if !allow_list.permits(host) {
        return Decision::Refuse(format!("{host} is not on this attempt's allow-list"));
    }
    let addresses: Vec<IpAddr> = match network.resolve(host, port) {
        Ok(addresses) if !addresses.is_empty() => addresses,
        Ok(_) | Err(_) => return Decision::Refuse(format!("{host} could not be resolved")),
    };
    if let Some(internal) = addresses.iter().find(|address| is_host_internal(**address)) {
        return Decision::Refuse(format!(
            "{host} resolves to {internal}, an address on this machine's own networks"
        ));
    }
    Decision::Tunnel(SocketAddr::new(addresses[0], port))
}

/// A running broker for one attempt.
#[derive(Debug)]
pub struct EgressBroker {
    address: SocketAddr,
}

impl EgressBroker {
    /// Starts a broker on a free loopback port. It runs until the process exits.
    ///
    /// # Errors
    ///
    /// Returns the error if the loopback listener cannot be opened.
    pub fn start(allow_list: AllowList, network: impl Network) -> io::Result<Self> {
        let listener: TcpListener = TcpListener::bind(("127.0.0.1", 0))?;
        let address: SocketAddr = listener.local_addr()?;
        let shared = std::sync::Arc::new((allow_list, network));
        thread::spawn(move || {
            for client in listener.incoming().flatten() {
                let shared = std::sync::Arc::clone(&shared);
                thread::spawn(move || {
                    let _ = serve(client, &shared.0, &shared.1);
                });
            }
        });
        Ok(Self { address })
    }

    /// Where agents send their proxy requests.
    #[must_use]
    pub fn address(&self) -> SocketAddr {
        self.address
    }
}

/// Handles one client: reads the request head, decides, then tunnels or refuses.
fn serve(client: TcpStream, allow_list: &AllowList, network: &dyn Network) -> io::Result<()> {
    let mut reader = BufReader::new(client.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut consumed: usize = request_line.len();
    loop {
        let mut header = String::new();
        let read: usize = reader.read_line(&mut header)?;
        consumed += read;
        if read == 0 || header == "\r\n" || header == "\n" || consumed > MAX_REQUEST_HEAD {
            break;
        }
    }
    let mut writer = client;
    let Some((host, port)) = connect_target(&request_line) else {
        writer.write_all(b"HTTP/1.1 405 Method Not Allowed\r\n\r\n")?;
        return Ok(());
    };
    match decide(allow_list, network, &host, port) {
        Decision::Refuse(reason) => {
            writer.write_all(
                format!("HTTP/1.1 403 Forbidden\r\nX-Kaisa-Reason: {reason}\r\n\r\n").as_bytes(),
            )?;
            Ok(())
        }
        Decision::Tunnel(address) => {
            let upstream: TcpStream = match network.connect(address) {
                Ok(upstream) => upstream,
                Err(_) => {
                    writer.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")?;
                    return Ok(());
                }
            };
            writer.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
            let mut upstream_reader = upstream.try_clone()?;
            let mut client_writer = writer.try_clone()?;
            let to_client =
                thread::spawn(move || io::copy(&mut upstream_reader, &mut client_writer));
            let mut upstream_writer = upstream;
            // Bytes the client sent after the head are already buffered in `reader`.
            let _ = io::copy(&mut reader, &mut upstream_writer);
            let _ = upstream_writer.shutdown(std::net::Shutdown::Write);
            let _ = to_client.join();
            Ok(())
        }
    }
}

/// `(host, port)` of a `CONNECT host:port HTTP/1.x` line; `None` for anything else.
fn connect_target(request_line: &str) -> Option<(String, u16)> {
    let mut parts = request_line.split_whitespace();
    if parts.next()? != "CONNECT" {
        return None;
    }
    let authority: &str = parts.next()?;
    let (host, port) = authority.rsplit_once(':')?;
    let host: &str = host.trim_start_matches('[').trim_end_matches(']');
    Some((host.to_owned(), port.parse().ok()?))
}

/// Reads exactly the bytes available after a successful CONNECT; used by tests.
#[cfg(test)]
fn read_reply_head(stream: &mut TcpStream) -> io::Result<String> {
    let mut head: Vec<u8> = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if io::Read::read(stream, &mut byte)? == 0 {
            break;
        }
        head.push(byte[0]);
    }
    Ok(String::from_utf8_lossy(&head).into_owned())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::{self, Read, Write};
    use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
    use std::thread;

    use super::{EgressBroker, Network, read_reply_head};
    use crate::allow_list::AllowList;

    /// An address from the documentation range: public-looking, never routed.
    const REGISTRY_ADDRESS: &str = "203.0.113.10";
    /// The HTTPS port.
    const HTTPS: u16 = 443;
    /// What the fake registry answers on every connection.
    const REGISTRY_GREETING: &[u8] = b"hello from the registry";

    /// A network where names resolve from a table and "public" addresses are served by local
    /// listeners, so tunnelling can be tested without the internet.
    struct FakeNetwork {
        names: HashMap<String, IpAddr>,
        servers: HashMap<IpAddr, SocketAddr>,
    }

    impl Network for FakeNetwork {
        fn resolve(&self, host: &str, _port: u16) -> io::Result<Vec<IpAddr>> {
            self.names
                .get(host)
                .map(|address| vec![*address])
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "unknown name"))
        }

        fn connect(&self, address: SocketAddr) -> io::Result<TcpStream> {
            let local: SocketAddr = *self
                .servers
                .get(&address.ip())
                .ok_or_else(|| io::Error::new(io::ErrorKind::ConnectionRefused, "no server"))?;
            TcpStream::connect(local)
        }
    }

    /// A registry that greets every client.
    fn a_greeting_registry() -> SocketAddr {
        let listener: TcpListener = TcpListener::bind(("127.0.0.1", 0)).expect("loopback works");
        let address: SocketAddr = listener.local_addr().expect("bound");
        thread::spawn(move || {
            for mut client in listener.incoming().flatten() {
                let _ = client.write_all(REGISTRY_GREETING);
            }
        });
        address
    }

    fn a_broker_allowing_the_registry(also_resolve: &[(&str, &str)]) -> EgressBroker {
        let registry: SocketAddr = a_greeting_registry();
        let registry_ip: IpAddr = REGISTRY_ADDRESS.parse().expect("valid");
        let mut names: HashMap<String, IpAddr> =
            HashMap::from([("registry.npmjs.org".to_owned(), registry_ip)]);
        for (name, address) in also_resolve {
            names.insert((*name).to_owned(), address.parse().expect("valid"));
        }
        EgressBroker::start(
            AllowList::new(&[".npmjs.org", "rebound.example"]),
            FakeNetwork {
                names,
                servers: HashMap::from([(registry_ip, registry)]),
            },
        )
        .expect("the broker starts")
    }

    fn connect_through(broker: &EgressBroker, target: &str) -> (String, TcpStream) {
        let mut agent: TcpStream = TcpStream::connect(broker.address()).expect("broker reachable");
        agent
            .write_all(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n").as_bytes())
            .expect("the request is sent");
        let head: String = read_reply_head(&mut agent).expect("the broker answers");
        (head, agent)
    }

    #[test]
    fn an_allowed_domain_is_tunnelled() {
        let broker: EgressBroker = a_broker_allowing_the_registry(&[]);

        let (head, mut tunnel) = connect_through(&broker, &format!("registry.npmjs.org:{HTTPS}"));
        let mut greeting = vec![0_u8; REGISTRY_GREETING.len()];
        tunnel
            .read_exact(&mut greeting)
            .expect("bytes flow through the tunnel");

        assert!(
            head.starts_with("HTTP/1.1 200"),
            "package installs must still work, got {head:?}"
        );
        assert_eq!(
            greeting, REGISTRY_GREETING,
            "the tunnel carries the registry's bytes"
        );
    }

    #[test]
    fn a_domain_off_the_list_is_refused() {
        let broker: EgressBroker = a_broker_allowing_the_registry(&[]);

        let (head, _) = connect_through(&broker, &format!("attacker.example:{HTTPS}"));

        assert!(
            head.starts_with("HTTP/1.1 403"),
            "an agent must not send code or secrets to an arbitrary host, got {head:?}"
        );
    }

    #[test]
    fn a_name_resolving_to_loopback_is_refused() {
        let broker: EgressBroker =
            a_broker_allowing_the_registry(&[("rebound.example", "127.0.0.1")]);

        let (head, _) = connect_through(&broker, &format!("rebound.example:{HTTPS}"));

        assert!(
            head.starts_with("HTTP/1.1 403"),
            "an allowed name pointed at this machine must not reach the console or control endpoint, got {head:?}"
        );
    }
}
