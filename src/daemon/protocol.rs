//! Length-prefixed JSON framing for the daemon protocol.
//!
//! Each frame: `[u32 big-endian payload length][payload bytes]`. The
//! payload limit defaults to 64 MiB (configurable per stream) so a
//! malformed peer can't allocate the whole address space. The functions
//! return typed `io::Result` so the daemon loop can use `?` on them.

use std::io;

use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Hard cap on a single frame's payload size. Refusing a frame larger
/// than this prevents allocating multi-GB buffers from a malicious peer.
pub const DEFAULT_MAX_PAYLOAD: usize = 64 * 1024 * 1024;

pub async fn read_frame<R: AsyncReadExt + Unpin, T: DeserializeOwned>(r: &mut R) -> io::Result<T> {
    read_frame_with_limit(r, DEFAULT_MAX_PAYLOAD).await
}

pub async fn read_frame_with_limit<R: AsyncReadExt + Unpin, T: DeserializeOwned>(
    r: &mut R,
    max_payload: usize,
) -> io::Result<T> {
    let buf = read_raw_frame_with_limit(r, max_payload).await?;
    serde_json::from_slice(&buf)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("JSON: {e}")))
}

/// Read one raw length-prefixed payload.
///
/// This preserves the daemon's `[u32 BE length][payload bytes]` framing
/// without deserializing the payload. Authenticated protocols must use this
/// helper so they can verify MACs before parsing untrusted body bytes.
pub async fn read_raw_frame_with_limit<R: AsyncReadExt + Unpin>(
    r: &mut R,
    max_payload: usize,
) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > max_payload {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame too large: {len} > {max_payload}"),
        ));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

pub async fn write_frame<W: AsyncWriteExt + Unpin, T: Serialize>(
    w: &mut W,
    msg: &T,
) -> io::Result<()> {
    let body = serde_json::to_vec(msg)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("serialize: {e}")))?;
    write_raw_frame(w, &body).await
}

/// Write one raw payload using the daemon's `[u32 BE length][payload bytes]`
/// framing.
pub async fn write_raw_frame<W: AsyncWriteExt + Unpin>(w: &mut W, body: &[u8]) -> io::Result<()> {
    let len = u32::try_from(body.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "payload exceeds u32 max"))?;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(body).await?;
    w.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use tokio::io::duplex;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Sample {
        x: u32,
        name: String,
    }

    #[tokio::test]
    async fn round_trip() {
        let (mut a, mut b) = duplex(4096);
        let msg = Sample {
            x: 42,
            name: "hi".into(),
        };
        let writer = async {
            write_frame(&mut a, &msg).await.unwrap();
        };
        let reader = async {
            let got: Sample = read_frame(&mut b).await.unwrap();
            assert_eq!(got, msg);
        };
        futures::future::join(writer, reader).await;
    }

    #[tokio::test]
    async fn rejects_oversize_frame() {
        let (mut a, mut b) = duplex(4096);
        // Manually send a fraudulent length prefix announcing 100 MB.
        let writer = async {
            a.write_all(&(100u32 * 1024 * 1024).to_be_bytes())
                .await
                .unwrap();
            a.flush().await.unwrap();
        };
        let reader = async {
            let res: io::Result<Sample> = read_frame_with_limit(&mut b, 1024).await;
            assert!(res.is_err());
            let e = res.unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        };
        futures::future::join(writer, reader).await;
    }
}
