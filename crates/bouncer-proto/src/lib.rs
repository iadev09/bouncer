use std::io::{Read, Write};

use serde::{Deserialize, Serialize};
use thiserror::Error;
#[cfg(feature = "tokio")]
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAGIC: [u8; 4] = *b"BNCE";
pub const ACK: &[u8; 3] = b"OK\n";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Header {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Error)]
pub enum ProtoError {
    #[error("invalid frame magic")]
    InvalidMagic,
    #[error("header too large: {0} bytes")]
    HeaderTooLarge(u32),
    #[error("body too large: {0} bytes")]
    BodyTooLarge(u64),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("header encode error: {0}")]
    HeaderEncode(String),
    #[error("header decode error: {0}")]
    HeaderDecode(String),
}

pub fn encode_header_json(header: &Header) -> Result<Vec<u8>, ProtoError> {
    serde_json::to_vec(header)
        .map_err(|err| ProtoError::HeaderEncode(err.to_string()))
}

pub fn decode_header_json(bytes: &[u8]) -> Result<Header, ProtoError> {
    serde_json::from_slice(bytes)
        .map_err(|err| ProtoError::HeaderDecode(err.to_string()))
}

pub fn write_frame_sync<W: Write>(
    writer: &mut W,
    header: &[u8],
    body: &[u8],
) -> Result<(), ProtoError> {
    let header_len = u32::try_from(header.len())
        .map_err(|_| ProtoError::HeaderTooLarge(u32::MAX))?;
    let body_len = u64::try_from(body.len())
        .map_err(|_| ProtoError::BodyTooLarge(u64::MAX))?;

    writer.write_all(&MAGIC)?;
    writer.write_all(&header_len.to_be_bytes())?;
    writer.write_all(&body_len.to_be_bytes())?;
    writer.write_all(header)?;
    writer.write_all(body)?;
    Ok(())
}

#[cfg(feature = "tokio")]
pub async fn write_frame_async<W: AsyncWrite + Unpin>(
    writer: &mut W,
    header: &[u8],
    body: &[u8],
) -> Result<(), ProtoError> {
    let header_len = u32::try_from(header.len())
        .map_err(|_| ProtoError::HeaderTooLarge(u32::MAX))?;
    let body_len = u64::try_from(body.len())
        .map_err(|_| ProtoError::BodyTooLarge(u64::MAX))?;

    writer.write_all(&MAGIC).await?;
    writer.write_all(&header_len.to_be_bytes()).await?;
    writer.write_all(&body_len.to_be_bytes()).await?;
    writer.write_all(header).await?;
    writer.write_all(body).await?;
    Ok(())
}

#[cfg(feature = "tokio")]
pub async fn read_frame_async<R: AsyncRead + Unpin>(
    reader: &mut R,
    max_header_len: u32,
    max_body_len: u64,
) -> Result<(Vec<u8>, Vec<u8>), ProtoError> {
    let mut magic = [0_u8; 4];
    reader.read_exact(&mut magic).await?;
    if magic != MAGIC {
        return Err(ProtoError::InvalidMagic);
    }

    let mut header_len_buf = [0_u8; 4];
    reader.read_exact(&mut header_len_buf).await?;
    let header_len = u32::from_be_bytes(header_len_buf);
    if header_len > max_header_len {
        return Err(ProtoError::HeaderTooLarge(header_len));
    }

    let mut body_len_buf = [0_u8; 8];
    reader.read_exact(&mut body_len_buf).await?;
    let body_len = u64::from_be_bytes(body_len_buf);
    if body_len > max_body_len {
        return Err(ProtoError::BodyTooLarge(body_len));
    }

    let mut header = vec![0_u8; header_len as usize];
    reader.read_exact(&mut header).await?;

    let mut body = vec![0_u8; body_len as usize];
    reader.read_exact(&mut body).await?;

    Ok((header, body))
}

pub fn read_ack_sync<R: Read>(reader: &mut R) -> Result<(), ProtoError> {
    let mut ack = [0_u8; 3];
    reader.read_exact(&mut ack)?;
    if ack == *ACK { Ok(()) } else { Err(ProtoError::InvalidMagic) }
}

#[cfg(feature = "tokio")]
pub async fn read_ack_async<R: AsyncRead + Unpin>(
    reader: &mut R
) -> Result<(), ProtoError> {
    let mut ack = [0_u8; 3];
    reader.read_exact(&mut ack).await?;
    if ack == *ACK { Ok(()) } else { Err(ProtoError::InvalidMagic) }
}
