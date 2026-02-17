use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const EX_USAGE: u8 = 64;
const EX_TEMPFAIL: u8 = 75;
const MAX_BODY_BYTES: usize = 25 * 1024 * 1024;
const MAX_QUEUE_ID_LEN: usize = 64;

static NONCE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let code = match err {
                DeliveryError::Usage(_) => EX_USAGE,
                DeliveryError::Runtime(_) => EX_TEMPFAIL
            };
            eprintln!("bounce-delivery error: {err}");
            ExitCode::from(code)
        }
    }
}

fn run() -> Result<()> {
    let args = Cli::parse(std::env::args().skip(1))?;
    let body = read_body(&mut io::stdin(), MAX_BODY_BYTES)?;
    write_incoming_mail(&args.incoming_dir, args.queue_id.as_deref(), &body)?;
    Ok(())
}

fn read_body<R: Read>(
    reader: &mut R,
    max_body_bytes: usize
) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    reader
        .take((max_body_bytes as u64) + 1)
        .read_to_end(&mut body)
        .map_err(|err| runtime_err("failed to read mail from stdin", err))?;

    if body.len() > max_body_bytes {
        return Err(DeliveryError::Runtime(format!(
            "mail body too large: max {} bytes",
            max_body_bytes
        )));
    }

    Ok(body)
}

fn write_incoming_mail(
    incoming_dir: &Path,
    queue_id: Option<&str>,
    body: &[u8]
) -> Result<PathBuf> {
    fs::create_dir_all(incoming_dir)
        .map_err(|err| runtime_err("failed to create incoming dir", err))?;

    let queue = sanitize_queue_id(queue_id.unwrap_or("na"));
    let (unix_ms, unix_ns) = unix_timestamps();
    let pid = process::id();
    let nonce = build_nonce_hex(unix_ns, pid, &queue);
    let base = format!("{unix_ms}-{pid}-{queue}-{nonce}");
    let final_path = incoming_dir.join(format!("{base}.eml"));
    let tmp_path = incoming_dir.join(format!(".{base}.tmp"));

    write_temp_and_rename(&tmp_path, &final_path, body)?;
    Ok(final_path)
}

fn write_temp_and_rename(
    tmp_path: &Path,
    final_path: &Path,
    body: &[u8]
) -> Result<()> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(tmp_path)
        .map_err(|err| runtime_err("failed to create temp file", err))?;

    if let Err(err) = file.write_all(body) {
        let _ = fs::remove_file(tmp_path);
        return Err(runtime_err("failed to write temp file", err));
    }

    if let Err(err) = file.sync_all() {
        let _ = fs::remove_file(tmp_path);
        return Err(runtime_err("failed to fsync temp file", err));
    }

    drop(file);

    fs::rename(tmp_path, final_path)
        .map_err(|err| runtime_err("failed to move temp file into incoming", err))
}

fn sanitize_queue_id(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(MAX_QUEUE_ID_LEN));

    for ch in raw.chars() {
        if out.len() >= MAX_QUEUE_ID_LEN {
            break;
        }

        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }

    if out.is_empty() {
        "na".to_string()
    } else {
        out
    }
}

fn unix_timestamps() -> (u128, u128) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (now.as_millis(), now.as_nanos())
}

fn build_nonce_hex(
    unix_ns: u128,
    pid: u32,
    queue_id: &str
) -> String {
    let seq = NONCE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut hasher = DefaultHasher::new();
    unix_ns.hash(&mut hasher);
    pid.hash(&mut hasher);
    seq.hash(&mut hasher);
    queue_id.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[derive(Debug)]
struct Cli {
    incoming_dir: PathBuf,
    queue_id: Option<String>
}

impl Cli {
    fn parse<I>(mut args: I) -> Result<Self>
    where
        I: Iterator<Item = String>
    {
        let mut incoming_dir: Option<PathBuf> = None;
        let mut queue_id: Option<String> = None;

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--incoming-dir" => {
                    let raw = args.next().ok_or_else(|| {
                        DeliveryError::Usage(
                            "missing value for --incoming-dir".to_string()
                        )
                    })?;
                    incoming_dir = Some(PathBuf::from(raw));
                }
                "--queue-id" => {
                    queue_id = Some(args.next().ok_or_else(|| {
                        DeliveryError::Usage(
                            "missing value for --queue-id".to_string()
                        )
                    })?);
                }
                "--from" | "--to" | "--original-to" | "--size" => {
                    let _ = args.next().ok_or_else(|| {
                        DeliveryError::Usage(format!(
                            "missing value for {arg}"
                        ))
                    })?;
                }
                "-h" | "--help" => {
                    return Err(DeliveryError::Usage(
                        "usage: bounce-delivery --incoming-dir PATH [--queue-id QUEUE_ID] [--from SENDER] [--to RECIPIENT] [--original-to RECIPIENT] [--size BYTES]"
                            .to_string(),
                    ));
                }
                _ => {
                    return Err(DeliveryError::Usage(format!(
                        "unknown argument: {arg}"
                    )));
                }
            }
        }

        Ok(Self {
            incoming_dir: incoming_dir.ok_or_else(|| {
                DeliveryError::Usage(
                    "missing required argument --incoming-dir".to_string()
                )
            })?,
            queue_id
        })
    }
}

#[derive(Debug)]
enum DeliveryError {
    Usage(String),
    Runtime(String)
}

impl fmt::Display for DeliveryError {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>
    ) -> fmt::Result {
        match self {
            DeliveryError::Usage(msg) => write!(f, "{msg}"),
            DeliveryError::Runtime(msg) => write!(f, "{msg}")
        }
    }
}

impl std::error::Error for DeliveryError {}

fn runtime_err(
    context: impl Into<String>,
    err: impl fmt::Display
) -> DeliveryError {
    DeliveryError::Runtime(format!("{}: {err}", context.into()))
}

type Result<T> = std::result::Result<T, DeliveryError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_queue_id_filters_chars_and_limits_length() {
        let raw = "ABC/123:queue with spaces and symbols !@#";
        let got = sanitize_queue_id(raw);
        assert_eq!(got, "ABC_123_queue_with_spaces_and_symbols____");
    }

    #[test]
    fn sanitize_queue_id_falls_back_to_na() {
        assert_eq!(sanitize_queue_id(""), "na");
    }

    #[test]
    fn nonce_is_hex_len_16() {
        let nonce = build_nonce_hex(1, 2, "QID");
        assert_eq!(nonce.len(), 16);
        assert!(nonce.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
