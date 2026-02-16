use std::fmt;
use std::io::{self, Read};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::process::ExitCode;
use std::time::Duration;

use bouncer_proto::{
    Header, encode_header_json, read_ack_sync, write_frame_sync,
};

const EX_TEMPFAIL: u8 = 75;
const EX_USAGE: u8 = 64;
const MAX_BODY_BYTES: usize = 50 * 1024;

type Result<T> = std::result::Result<T, ClientError>;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let code = match err {
                ClientError::Usage(_) => EX_USAGE,
                ClientError::Runtime(_) => EX_TEMPFAIL,
            };
            eprintln!("bouncer-client error: {err}");
            ExitCode::from(code)
        }
    }
}

fn run() -> Result<()> {
    let args = Cli::parse(std::env::args().skip(1))?;
    run_with_cli(args, &mut io::stdin())
}

fn run_with_cli<R: Read>(
    args: Cli,
    stdin: &mut R,
) -> Result<()> {
    let body = read_body(stdin, MAX_BODY_BYTES)?;
    let header_bytes = build_header_bytes(&args)?;
    let timeout = Duration::from_secs(args.timeout_secs);
    let addr = resolve_socket_addr(&args.server)?;
    send_frame_and_wait_ack(addr, timeout, &header_bytes, &body)
}

fn read_body<R: Read>(
    reader: &mut R,
    max_body_bytes: usize,
) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    reader
        .take((max_body_bytes as u64) + 1)
        .read_to_end(&mut body)
        .map_err(|err| runtime_err("failed to read mail from stdin", err))?;
    if body.len() > max_body_bytes {
        return Err(ClientError::Runtime(format!(
            "mail body too large: max {} bytes",
            max_body_bytes
        )));
    }
    Ok(body)
}

fn build_header_bytes(args: &Cli) -> Result<Vec<u8>> {
    let header = Header {
        from: args.from.clone(),
        to: args.to.clone(),
        kind: None,
        source: None,
    };
    let header_bytes = encode_header_json(&header)
        .map_err(|err| runtime_err("failed to serialize header", err))?;
    Ok(header_bytes)
}

fn send_frame_and_wait_ack(
    addr: SocketAddr,
    timeout: Duration,
    header_bytes: &[u8],
    body: &[u8],
) -> Result<()> {
    let mut stream =
        TcpStream::connect_timeout(&addr, timeout).map_err(|err| {
            runtime_err(format!("failed to connect to {}", addr), err)
        })?;
    stream.set_nodelay(true).ok();

    stream
        .set_write_timeout(Some(timeout))
        .map_err(|err| runtime_err("failed to set write timeout", err))?;

    stream
        .set_read_timeout(Some(timeout))
        .map_err(|err| runtime_err("failed to set read timeout", err))?;

    write_frame_sync(&mut stream, header_bytes, body)
        .map_err(|err| runtime_err("failed to send frame", err))?;

    read_ack_sync(&mut stream)
        .map_err(|err| runtime_err("invalid/missing ACK from server", err))?;

    Ok(())
}

fn resolve_socket_addr(server: &str) -> Result<SocketAddr> {
    server
        .to_socket_addrs()
        .map_err(|err| {
            runtime_err(
                format!("failed to resolve server address: {server}"),
                err,
            )
        })?
        .next()
        .ok_or_else(|| {
            ClientError::Runtime(format!(
                "no address resolved for server: {server}"
            ))
        })
}

#[derive(Debug)]
struct Cli {
    server: String,
    from: String,
    to: String,
    timeout_secs: u64,
}

impl Cli {
    fn parse<I>(mut args: I) -> Result<Self>
    where
        I: Iterator<Item = String>,
    {
        let mut server = None;
        let mut from = None;
        let mut to = None;
        let mut timeout_secs = 10_u64;

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--server" => server = args.next(),
                "--from" => from = args.next(),
                "--to" => to = args.next(),
                "--timeout-secs" => {
                    let raw = args.next().ok_or_else(|| {
                        ClientError::Usage(
                            "missing value for --timeout-secs".to_string(),
                        )
                    })?;
                    timeout_secs = raw.parse::<u64>().map_err(|_| {
                        ClientError::Usage(
                            "--timeout-secs must be a positive integer"
                                .to_string(),
                        )
                    })?;
                }
                "-h" | "--help" => {
                    return Err(ClientError::Usage(
                        "usage: bouncer-client --server host:port --from sender --to recipient [--timeout-secs 10]"
                            .to_string(),
                    ));
                }
                _ => {
                    return Err(ClientError::Usage(format!(
                        "unknown argument: {arg}"
                    )));
                }
            }
        }

        Ok(Self {
            server: server.ok_or_else(|| {
                ClientError::Usage(
                    "missing required argument --server".to_string(),
                )
            })?,
            from: from.ok_or_else(|| {
                ClientError::Usage(
                    "missing required argument --from".to_string(),
                )
            })?,
            to: to.ok_or_else(|| {
                ClientError::Usage("missing required argument --to".to_string())
            })?,
            timeout_secs,
        })
    }
}

#[derive(Debug)]
enum ClientError {
    Usage(String),
    Runtime(String),
}

impl fmt::Display for ClientError {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            ClientError::Usage(msg) => write!(f, "{msg}"),
            ClientError::Runtime(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ClientError {}

fn runtime_err(
    context: impl Into<String>,
    err: impl fmt::Display,
) -> ClientError {
    ClientError::Runtime(format!("{}: {err}", context.into()))
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use bouncer_proto::{ACK, MAGIC, decode_header_json};

    use super::{
        Cli, ClientError, build_header_bytes, read_body, run_with_cli,
    };

    #[test]
    fn cli_parse_success() {
        let args = vec![
            "--server".to_string(),
            "127.0.0.1:2147".to_string(),
            "--from".to_string(),
            "sender@example.com".to_string(),
            "--to".to_string(),
            "bounces@example.com".to_string(),
            "--timeout-secs".to_string(),
            "3".to_string(),
        ];
        let cli = Cli::parse(args.into_iter()).expect("parse should succeed");
        assert_eq!(cli.server, "127.0.0.1:2147");
        assert_eq!(cli.from, "sender@example.com");
        assert_eq!(cli.to, "bounces@example.com");
        assert_eq!(cli.timeout_secs, 3);
    }

    #[test]
    fn cli_parse_missing_required_argument() {
        let err = Cli::parse(
            vec![
                "--from".to_string(),
                "sender@example.com".to_string(),
                "--to".to_string(),
                "bounces@example.com".to_string(),
            ]
            .into_iter(),
        )
        .expect_err("parse should fail");

        match err {
            ClientError::Usage(msg) => {
                assert!(msg.contains("missing required argument --server"));
            }
            _ => panic!("expected usage error"),
        }
    }

    #[test]
    fn read_body_respects_limit() {
        let mut input = Cursor::new(b"012345".to_vec());
        let err = read_body(&mut input, 5).expect_err("should fail on limit");
        match err {
            ClientError::Runtime(msg) => {
                assert!(msg.contains("mail body too large: max 5 bytes"));
            }
            _ => panic!("expected runtime error"),
        }
    }

    #[test]
    fn build_header_bytes_contains_expected_fields() {
        let cli = Cli {
            server: "127.0.0.1:2147".to_string(),
            from: "sender@example.com".to_string(),
            to: "bounces@example.com".to_string(),
            timeout_secs: 10,
        };
        let encoded = build_header_bytes(&cli).expect("header build");
        let decoded = decode_header_json(&encoded).expect("header decode");
        assert_eq!(decoded.from, "sender@example.com");
        assert_eq!(decoded.to, "bounces@example.com");
        assert!(decoded.kind.is_none());
        assert!(decoded.source.is_none());
    }

    #[test]
    fn run_with_cli_sends_fixture_and_receives_ack() {
        let fixture = fixture_bytes();
        let Some(listener) = bind_local_listener_or_skip() else {
            return;
        };
        let addr = listener.local_addr().expect("local addr");

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let (header, body) = read_frame_sync(&mut stream).expect("frame");
            let decoded = decode_header_json(&header).expect("decode header");
            assert_eq!(decoded.from, "sender@example.com");
            assert_eq!(decoded.to, "bounces@example.com");
            assert_eq!(body, fixture);
            stream.write_all(ACK).expect("ack write");
        });

        let cli = Cli {
            server: addr.to_string(),
            from: "sender@example.com".to_string(),
            to: "bounces@example.com".to_string(),
            timeout_secs: 3,
        };
        let mut stdin = Cursor::new(fixture_bytes());
        run_with_cli(cli, &mut stdin).expect("client run should succeed");
        handle.join().expect("server thread join");
    }

    #[test]
    fn run_with_cli_fails_when_ack_is_missing() {
        let Some(listener) = bind_local_listener_or_skip() else {
            return;
        };
        let addr = listener.local_addr().expect("local addr");

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let _ = read_frame_sync(&mut stream).expect("frame");
            // Intentionally close without ACK.
        });

        let cli = Cli {
            server: addr.to_string(),
            from: "sender@example.com".to_string(),
            to: "bounces@example.com".to_string(),
            timeout_secs: 1,
        };
        let mut stdin = Cursor::new(fixture_bytes());
        let err = run_with_cli(cli, &mut stdin).expect_err("must fail");
        match err {
            ClientError::Runtime(msg) => {
                assert!(msg.contains("invalid/missing ACK from server"));
            }
            _ => panic!("expected runtime error"),
        }
        handle.join().expect("server thread join");
    }

    fn fixture_bytes() -> Vec<u8> {
        include_bytes!("../../../tests/bounces/notification.eml").to_vec()
    }

    fn bind_local_listener_or_skip() -> Option<TcpListener> {
        match TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => Some(listener),
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping network test: {err}");
                None
            }
            Err(err) => panic!("bind test listener failed: {err}"),
        }
    }

    fn read_frame_sync<R: Read>(
        reader: &mut R
    ) -> std::io::Result<(Vec<u8>, Vec<u8>)> {
        let mut magic = [0u8; 4];
        reader.read_exact(&mut magic)?;
        if magic != MAGIC {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid frame magic",
            ));
        }

        let mut header_len_buf = [0u8; 4];
        reader.read_exact(&mut header_len_buf)?;
        let header_len = u32::from_be_bytes(header_len_buf) as usize;

        let mut body_len_buf = [0u8; 8];
        reader.read_exact(&mut body_len_buf)?;
        let body_len = u64::from_be_bytes(body_len_buf) as usize;

        let mut header = vec![0u8; header_len];
        reader.read_exact(&mut header)?;
        let mut body = vec![0u8; body_len];
        reader.read_exact(&mut body)?;

        Ok((header, body))
    }
}
