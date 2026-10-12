//! Length-prefixed JSON framing.

use std::io::{self, Read, Write};

use serde::{de::DeserializeOwned, Serialize};

/// Maximum frame body size (16 MiB).
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

fn too_big(n: usize) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("frame of {n} bytes exceeds {MAX_FRAME}"))
}

/// Write one frame (`u32` LE length + body) and flush.
pub fn write_frame<W: Write>(w: &mut W, body: &[u8]) -> io::Result<()> {
    if body.len() > MAX_FRAME {
        return Err(too_big(body.len()));
    }
    w.write_all(&(body.len() as u32).to_le_bytes())?;
    w.write_all(body)?;
    w.flush()
}

/// Read one frame. Oversize lengths are rejected before allocating.
/// EOF (clean or mid-frame) yields `ErrorKind::UnexpectedEof`.
pub fn read_frame<R: Read>(r: &mut R) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let n = u32::from_le_bytes(len) as usize;
    if n > MAX_FRAME {
        return Err(too_big(n));
    }
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

pub fn write_message<W: Write, T: Serialize>(w: &mut W, msg: &T) -> io::Result<()> {
    let body = serde_json::to_vec(msg).map_err(io::Error::other)?;
    write_frame(w, &body)
}

pub fn read_message<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<T> {
    let body = read_frame(r)?;
    serde_json::from_slice(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Request, Sort};

    #[test]
    fn roundtrip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"hello").unwrap();
        write_frame(&mut buf, b"").unwrap();
        let mut c = io::Cursor::new(buf);
        assert_eq!(read_frame(&mut c).unwrap(), b"hello");
        assert_eq!(read_frame(&mut c).unwrap(), b"");
        assert_eq!(read_frame(&mut c).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn message_roundtrip() {
        let m = Request::Search { id: 9, query: "x".into(), offset: 1, limit: 2, sort: Sort::NameAsc };
        let mut buf = Vec::new();
        write_message(&mut buf, &m).unwrap();
        assert_eq!(read_message::<_, Request>(&mut io::Cursor::new(buf)).unwrap(), m);
    }

    #[test]
    fn oversize_rejected() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&((MAX_FRAME as u32) + 1).to_le_bytes());
        let e = read_frame(&mut io::Cursor::new(buf)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        let big = vec![0u8; MAX_FRAME + 1];
        assert_eq!(write_frame(&mut Vec::new(), &big).unwrap_err().kind(), io::ErrorKind::InvalidData);
        let ok = vec![1u8; MAX_FRAME];
        let mut out = Vec::new();
        write_frame(&mut out, &ok).unwrap();
        assert_eq!(read_frame(&mut io::Cursor::new(out)).unwrap().len(), MAX_FRAME);
    }

    #[test]
    fn truncated() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"abcdef").unwrap();
        buf.truncate(buf.len() - 2);
        assert_eq!(read_frame(&mut io::Cursor::new(buf)).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(read_frame(&mut io::Cursor::new(vec![1u8, 0])).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn bad_json() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"{nope").unwrap();
        let e = read_message::<_, Request>(&mut io::Cursor::new(buf)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    }
}
