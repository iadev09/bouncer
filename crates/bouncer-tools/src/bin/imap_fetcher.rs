use std::env;
use std::fmt;
use std::path::PathBuf;

use anyhow::{Context, Result};
use async_imap::{Client, types::Uid};
use async_native_tls::TlsConnector;
use futures_util::TryStreamExt;
use tokio::net::TcpStream;

// BODY.PEEK[] reads message content without setting the \Seen flag.
const FETCH_QUERY: &str = "(UID BODY.PEEK[])";

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse(env::args().skip(1))?;
    println!(
        "imap_fetcher start: {} mode=peek_no_seen fetch_query={}",
        args, FETCH_QUERY
    );

    let tcp = TcpStream::connect((args.host.as_str(), args.port))
        .await
        .with_context(|| {
            format!("imap tcp connect failed: {}:{}", args.host, args.port)
        })?;
    let tls = TlsConnector::new();
    let tls_stream =
        tls.connect(args.host.as_str(), tcp).await.with_context(|| {
            format!("imap tls handshake failed: {}:{}", args.host, args.port)
        })?;

    let mut client = Client::new(tls_stream);
    client
        .read_response()
        .await
        .context("failed to read imap greeting")?
        .context("unexpected EOF while waiting IMAP greeting")?;

    let mut session = client
        .login(args.user.as_str(), args.pass.as_str())
        .await
        .map_err(|(err, _client)| err)
        .with_context(|| {
            format!("imap login failed: host={}, user={}", args.host, args.user)
        })?;

    session.select(&args.mailbox).await.with_context(|| {
        format!("imap select mailbox failed: {}", args.mailbox)
    })?;

    let mut uids: Vec<Uid> = session
        .uid_search(&args.search)
        .await
        .with_context(|| format!("imap uid search failed: {}", args.search))?
        .into_iter()
        .collect();

    uids.sort_unstable_by(|a, b| b.cmp(a));
    if args.limit > 0 {
        uids.truncate(args.limit);
    }

    tokio::fs::create_dir_all(&args.output_dir).await.with_context(|| {
        format!("failed to create output dir {}", args.output_dir.display())
    })?;

    if uids.is_empty() {
        println!("no messages matched search={}", args.search);
        session.logout().await.ok();
        return Ok(());
    }

    let uid_set = uids.iter().map(Uid::to_string).collect::<Vec<_>>().join(",");
    let mut fetches = session
        .uid_fetch(uid_set, FETCH_QUERY)
        .await
        .context("imap uid fetch failed")?;

    let mut saved = 0usize;
    while let Some(fetch) =
        fetches.try_next().await.context("imap fetch stream failed")?
    {
        let Some(uid) = fetch.uid else {
            continue;
        };
        let Some(body) = fetch.body() else {
            continue;
        };

        let path = args.output_dir.join(format!("uid-{uid}.eml"));
        tokio::fs::write(&path, body)
            .await
            .with_context(|| format!("failed to write {}", path.display()))?;
        saved += 1;
        println!(
            "saved uid={} bytes={} path={}",
            uid,
            body.len(),
            path.display()
        );
    }
    drop(fetches);

    session.logout().await.ok();
    println!(
        "completed: search={}, selected={}, saved={}, output_dir={}",
        args.search,
        uids.len(),
        saved,
        args.output_dir.display()
    );
    Ok(())
}

#[derive(Debug, Clone)]
struct Args {
    host: String,
    port: u16,
    user: String,
    pass: String,
    mailbox: String,
    search: String,
    limit: usize,
    output_dir: PathBuf,
}

impl Args {
    fn parse<I>(mut it: I) -> Result<Self>
    where
        I: Iterator<Item = String>,
    {
        let mut host = None;
        let mut port = 993u16;
        let mut user = None;
        let mut pass = None;
        let mut mailbox = "INBOX".to_string();
        let mut search = "UNSEEN".to_string();
        let mut limit = 50usize;
        let mut output_dir = PathBuf::from("tests/bounces");

        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--host" => host = it.next(),
                "--port" => {
                    let raw = it.next().context("missing value for --port")?;
                    port =
                        raw.parse::<u16>().context("invalid --port value")?;
                }
                "--user" => user = it.next(),
                "--pass" => pass = it.next(),
                "--mailbox" => {
                    mailbox =
                        it.next().context("missing value for --mailbox")?;
                }
                "--search" => {
                    search = it.next().context("missing value for --search")?;
                }
                "--limit" => {
                    let raw = it.next().context("missing value for --limit")?;
                    limit = raw
                        .parse::<usize>()
                        .context("invalid --limit value")?;
                }
                "--output-dir" => {
                    output_dir = PathBuf::from(
                        it.next().context("missing value for --output-dir")?,
                    );
                }
                "-h" | "--help" => {
                    print_usage();
                    std::process::exit(0);
                }
                _ => return Err(anyhow::anyhow!("unknown argument: {arg}")),
            }
        }

        Ok(Self {
            host: host.context("missing --host")?,
            port,
            user: user.context("missing --user")?,
            pass: pass.context("missing --pass")?,
            mailbox,
            search,
            limit,
            output_dir,
        })
    }
}

fn print_usage() {
    eprintln!(
        "usage: imap_fetcher --host HOST --user USER --pass PASS [--port 993] [--mailbox INBOX] [--search UNSEEN] [--limit 50] [--output-dir tests/bounces]"
    );
}

impl fmt::Display for Args {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        write!(
            f,
            "host={}, port={}, user={}, mailbox={}, search={}, limit={}, output_dir={}",
            self.host,
            self.port,
            self.user,
            self.mailbox,
            self.search,
            self.limit,
            self.output_dir.display()
        )
    }
}
