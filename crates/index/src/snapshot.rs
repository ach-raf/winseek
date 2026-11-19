//! Snapshot: `WSIX` v2, little endian, checksummed.
//!
//! ```text
//! magic "WSIX" | version u32 | flags u32 (bit0 = size/mtime columns) | nvols u32 | nrec u32 | norder u32
//! per volume: letter u8 | serial u64 | journal_id u64 | next_usn i64 | root u32 | kind u8 | next_id u64
//! parent u32[nrec] | vol u8[nrec] | flags u8[nrec] | frn u64[nrec] | (size u64[nrec] | mtime i64[nrec])
//! names_len u64 | front-coded names in STORAGE order: per record varint(shared) varint(suffix_len) bytes
//! order u32[norder]   (name sort array, deleted/stale/pending already folded in)
//! nids u32 | per interned ReFS FRN of a live folder: vol u8 | frn u128 | id u64
//! checksum u64 (over everything before it)
//! ```
//! v1 (no kind/next_id per volume, no ids section) still loads: every volume is NTFS.
//! Deleted slots are stored (empty name, DELETED flag) so record indices stay stable.
//! The FRN folder map, child links and hash table are rebuilt on load / lazily.

#![allow(clippy::chunks_exact_to_as_chunks)]

use crate::store::*;
use std::fmt;
use std::io;
use std::path::Path;

const MAGIC: &[u8; 4] = b"WSIX";
const VERSION: u32 = 2;
const VERSION_V1: u32 = 1;

#[derive(Debug)]
pub enum SnapshotError {
    Io(io::Error),
    Corrupt(&'static str),
}
impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SnapshotError::Io(e) => write!(f, "io: {e}"),
            SnapshotError::Corrupt(m) => write!(f, "corrupt snapshot: {m}"),
        }
    }
}
impl std::error::Error for SnapshotError {}
impl From<io::Error> for SnapshotError {
    fn from(e: io::Error) -> Self {
        SnapshotError::Io(e)
    }
}

fn checksum(data: &[u8]) -> u64 {
    let mut h = 0x9E37_79B9_7F4A_7C15u64;
    let mut it = data.chunks_exact(8);
    for c in &mut it {
        h = (h ^ u64::from_le_bytes(c.try_into().unwrap())).wrapping_mul(0xff51_afd7_ed55_8ccd);
        h ^= h >> 32;
    }
    for &b in it.remainder() {
        h = (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h ^ data.len() as u64
}

#[cfg(test)]
pub(crate) fn checksum_for_tests(d: &[u8]) -> u64 {
    checksum(d)
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

struct Rd<'a> {
    d: &'a [u8],
    p: usize,
}
impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], SnapshotError> {
        let end = self
            .p
            .checked_add(n)
            .filter(|&e| e <= self.d.len())
            .ok_or(SnapshotError::Corrupt("truncated"))?;
        let s = &self.d[self.p..end];
        self.p = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, SnapshotError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, SnapshotError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, SnapshotError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn u128(&mut self) -> Result<u128, SnapshotError> {
        Ok(u128::from_le_bytes(self.take(16)?.try_into().unwrap()))
    }
    fn varint(&mut self) -> Result<u64, SnapshotError> {
        let (mut v, mut shift) = (0u64, 0);
        loop {
            let b = self.u8()?;
            if shift >= 63 {
                return Err(SnapshotError::Corrupt("varint"));
            }
            v |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
            shift += 7;
        }
    }
    fn u32s(&mut self, n: usize) -> Result<Vec<u32>, SnapshotError> {
        Ok(self
            .take(n.checked_mul(4).ok_or(SnapshotError::Corrupt("len"))?)?
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect())
    }
    fn u64s(&mut self, n: usize) -> Result<Vec<u64>, SnapshotError> {
        Ok(self
            .take(n.checked_mul(8).ok_or(SnapshotError::Corrupt("len"))?)?
            .chunks_exact(8)
            .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
            .collect())
    }
}

/// Incremental form of [`checksum`]: feeding the same bytes in any split gives the same value.
struct Ck {
    h: u64,
    carry: [u8; 8],
    nc: usize,
    len: u64,
}

impl Ck {
    fn new() -> Self {
        Ck {
            h: 0x9E37_79B9_7F4A_7C15,
            carry: [0; 8],
            nc: 0,
            len: 0,
        }
    }
    fn eat(&mut self, c: [u8; 8]) {
        self.h = (self.h ^ u64::from_le_bytes(c)).wrapping_mul(0xff51_afd7_ed55_8ccd);
        self.h ^= self.h >> 32;
    }
    fn update(&mut self, mut d: &[u8]) {
        self.len += d.len() as u64;
        if self.nc > 0 {
            let take = (8 - self.nc).min(d.len());
            self.carry[self.nc..self.nc + take].copy_from_slice(&d[..take]);
            self.nc += take;
            d = &d[take..];
            if self.nc < 8 {
                return;
            }
            let c = self.carry;
            self.eat(c);
            self.nc = 0;
        }
        let mut it = d.chunks_exact(8);
        for c in &mut it {
            self.eat(c.try_into().unwrap());
        }
        let r = it.remainder();
        self.carry[..r.len()].copy_from_slice(r);
        self.nc = r.len();
    }
    fn finish(mut self) -> u64 {
        for i in 0..self.nc {
            self.h = (self.h ^ self.carry[i] as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
        self.h ^ self.len
    }
}

/// Buffered, checksumming output: sections are appended to `buf`, which goes to `w` in ~1 MiB
/// pieces, so a snapshot of a 15M-record index never exists in memory as one allocation.
struct Sink<W: io::Write> {
    w: W,
    ck: Ck,
    buf: Vec<u8>,
}

const SINK_BUF: usize = 1 << 20;

impl<W: io::Write> Sink<W> {
    fn new(w: W) -> Self {
        Sink {
            w,
            ck: Ck::new(),
            buf: Vec::with_capacity(SINK_BUF + 64),
        }
    }
    fn flush_buf(&mut self) -> io::Result<()> {
        self.ck.update(&self.buf);
        self.w.write_all(&self.buf)?;
        self.buf.clear();
        Ok(())
    }
    fn room(&mut self) -> io::Result<()> {
        if self.buf.len() >= SINK_BUF {
            self.flush_buf()?;
        }
        Ok(())
    }
    /// Flush, then append the checksum of everything written so far.
    fn finish(mut self) -> io::Result<W> {
        self.flush_buf()?;
        let ck = self.ck.finish();
        self.w.write_all(&ck.to_le_bytes())?;
        Ok(self.w)
    }
}

fn varint_len(mut v: u64) -> u64 {
    let mut n = 1;
    while v >= 0x80 {
        n += 1;
        v >>= 7;
    }
    n
}

impl Index {
    fn write_snapshot<W: io::Write>(&self, out: &mut Sink<W>) -> io::Result<()> {
        let order = self.order_vec();
        let n = self.parent.len();
        let b = &mut out.buf;
        b.extend_from_slice(MAGIC);
        b.extend_from_slice(&VERSION.to_le_bytes());
        b.extend_from_slice(&(self.with_meta as u32).to_le_bytes());
        b.extend_from_slice(&(self.vols.len() as u32).to_le_bytes());
        b.extend_from_slice(&(n as u32).to_le_bytes());
        b.extend_from_slice(&(order.len() as u32).to_le_bytes());
        for v in &self.vols {
            b.push(v.letter);
            b.extend_from_slice(&v.serial.to_le_bytes());
            b.extend_from_slice(&v.journal_id.to_le_bytes());
            b.extend_from_slice(&v.next_usn.to_le_bytes());
            b.extend_from_slice(&v.root.to_le_bytes());
            b.push(v.kind as u8);
            b.extend_from_slice(&v.next_id.to_le_bytes());
        }
        for &p in &self.parent {
            out.buf.extend_from_slice(&p.to_le_bytes());
            out.room()?;
        }
        out.buf.extend_from_slice(&self.vol);
        out.room()?;
        for &f in &self.flags {
            out.buf.push(f & F_PERSIST);
            out.room()?;
        }
        for &f in &self.frn {
            out.buf.extend_from_slice(&f.to_le_bytes());
            out.room()?;
        }
        if self.with_meta {
            for &s in &self.size {
                out.buf.extend_from_slice(&s.to_le_bytes());
                out.room()?;
            }
            for &m in &self.mtime {
                out.buf.extend_from_slice(&m.to_le_bytes());
                out.room()?;
            }
        }
        // Front-coded names. The section length comes first, so one cheap pass counts it.
        let shared_with =
            |prev: &[u8], cur: &[u8]| prev.iter().zip(cur).take_while(|(a, b)| a == b).count();
        let mut names_len = 0u64;
        let mut prev: &[u8] = &[];
        for r in 0..n as u32 {
            let cur = self.name_bytes(r);
            let shared = shared_with(prev, cur);
            names_len += varint_len(shared as u64)
                + varint_len((cur.len() - shared) as u64)
                + (cur.len() - shared) as u64;
            prev = cur;
        }
        out.buf.extend_from_slice(&names_len.to_le_bytes());
        let mut prev: &[u8] = &[];
        for r in 0..n as u32 {
            let cur = self.name_bytes(r);
            let shared = shared_with(prev, cur);
            put_varint(&mut out.buf, shared as u64);
            put_varint(&mut out.buf, (cur.len() - shared) as u64);
            out.buf.extend_from_slice(&cur[shared..]);
            prev = cur;
            out.room()?;
        }
        for &o in order.iter() {
            out.buf.extend_from_slice(&o.to_le_bytes());
            out.room()?;
        }
        let mut ids: Vec<(u8, u128, u64)> = self.live_ids128().collect();
        ids.sort_unstable();
        out.buf.extend_from_slice(&(ids.len() as u32).to_le_bytes());
        for (v, f, id) in ids {
            out.buf.push(v);
            out.buf.extend_from_slice(&f.to_le_bytes());
            out.buf.extend_from_slice(&id.to_le_bytes());
            out.room()?;
        }
        Ok(())
    }

    /// Serialize to bytes (folds pending changes into the saved order without mutating).
    /// Tests and tools only: the daemon streams through [`Index::save`].
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Sink::new(Vec::new());
        self.write_snapshot(&mut out)
            .expect("writing to a Vec cannot fail");
        out.finish().expect("writing to a Vec cannot fail")
    }

    /// Stream to `<path>.tmp`, then rename over `path`. Peak extra memory is ~1 MiB plus the
    /// (borrowed or merged) sort order, not a second copy of the whole index.
    pub fn save(&self, path: &Path) -> Result<(), SnapshotError> {
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = std::path::PathBuf::from(tmp);
        {
            let f = std::fs::File::create(&tmp)?;
            let mut out = Sink::new(f);
            self.write_snapshot(&mut out)?;
            let f = out.finish()?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Index, SnapshotError> {
        Self::from_bytes(&std::fs::read(path)?)
    }

    /// Parse and validate; never panics on malformed input.
    pub fn from_bytes(data: &[u8]) -> Result<Index, SnapshotError> {
        let c = SnapshotError::Corrupt;
        if data.len() < 32 {
            return Err(c("too short"));
        }
        let (body, ck) = data.split_at(data.len() - 8);
        if checksum(body) != u64::from_le_bytes(ck.try_into().unwrap()) {
            return Err(c("checksum"));
        }
        let mut r = Rd { d: body, p: 0 };
        if r.take(4)? != MAGIC {
            return Err(c("magic"));
        }
        let version = r.u32()?;
        if version != VERSION && version != VERSION_V1 {
            return Err(c("version"));
        }
        let v2 = version == VERSION;
        let fl = r.u32()?;
        let with_meta = fl & 1 != 0;
        let nvols = r.u32()? as usize;
        let n = r.u32()? as usize;
        let norder = r.u32()? as usize;
        let per = 4 + 1 + 1 + 8 + if with_meta { 16 } else { 0 };
        if nvols > 256
            || norder > n
            || n >= (u32::MAX - 2) as usize
            || n.saturating_mul(per) > body.len()
        {
            return Err(c("counts"));
        }
        let mut vols = Vec::with_capacity(nvols);
        for _ in 0..nvols {
            let letter = r.u8()?;
            let serial = r.u64()?;
            let journal_id = r.u64()?;
            let next_usn = r.u64()? as i64;
            let root = r.u32()?;
            if root as usize >= n {
                return Err(c("volume root"));
            }
            let (kind, next_id) = if v2 {
                (VolKind::from_u8(r.u8()?).ok_or(c("volume kind"))?, r.u64()?)
            } else {
                (VolKind::Ntfs, 0)
            };
            vols.push(VolumeMeta {
                letter,
                serial,
                journal_id,
                next_usn,
                root,
                kind,
                next_id,
            });
        }
        let parent = r.u32s(n)?;
        let vol = r.take(n)?.to_vec();
        let mut flags = r.take(n)?.to_vec();
        let frn = r.u64s(n)?;
        let (size, mtime) = if with_meta {
            (
                r.u64s(n)?,
                r.u64s(n)?.into_iter().map(|x| x as i64).collect(),
            )
        } else {
            (Vec::new(), Vec::new())
        };
        let names_len = r.u64()? as usize;
        let mut nr = Rd {
            d: r.take(names_len)?,
            p: 0,
        };
        let mut arena: Vec<u8> = Vec::with_capacity(names_len);
        let mut name_off = Vec::with_capacity(n);
        let mut name_len = Vec::with_capacity(n);
        let (mut poff, mut plen) = (0usize, 0usize);
        for _ in 0..n {
            let shared = nr.varint()? as usize;
            let suffix = nr.varint()? as usize;
            if shared > plen || shared.saturating_add(suffix) > u16::MAX as usize {
                return Err(c("name lengths"));
            }
            let sbytes = nr.take(suffix)?;
            let off = arena.len();
            if off + shared + suffix > u32::MAX as usize {
                return Err(c("arena"));
            }
            arena.extend_from_within(poff..poff + shared);
            arena.extend_from_slice(sbytes);
            if std::str::from_utf8(&arena[off..]).is_err() {
                return Err(c("utf8"));
            }
            name_off.push(off as u32);
            name_len.push((shared + suffix) as u16);
            poff = off;
            plen = shared + suffix;
        }
        let order = r.u32s(norder)?;
        let mut ids128 = Ids128::default();
        if v2 {
            let nids = r.u32()? as usize;
            if nids.saturating_mul(25) > body.len() - r.p {
                return Err(c("ids count"));
            }
            for _ in 0..nids {
                let (v, f, id) = (r.u8()?, r.u128()?, r.u64()?);
                ids128.insert((v, f), id);
            }
        }
        if r.p != body.len() {
            return Err(c("trailing bytes"));
        }
        // structural validation
        for i in 0..n {
            let f = flags[i];
            if f & !F_PERSIST != 0 {
                return Err(c("flags"));
            }
            if f & F_DELETED != 0 {
                continue;
            }
            let p = parent[i];
            if f & F_ROOT != 0 {
                if p != NONE || f & F_DIR == 0 {
                    return Err(c("root"));
                }
            } else {
                if p as usize >= n
                    || flags[p as usize] & (F_DIR | F_DELETED) != F_DIR
                    || p as usize == i
                {
                    return Err(c("parent"));
                }
            }
        }
        for v in &vols {
            if flags[v.root as usize] & (F_ROOT | F_DELETED) != F_ROOT {
                return Err(c("volume root flags"));
            }
        }
        for &o in &order {
            let ou = o as usize;
            if ou >= n
                || flags[ou] & (F_DELETED | F_ROOT | F_STUB) != 0
                || flags[ou] & F_INORDER != 0
            {
                return Err(c("order"));
            }
            flags[ou] |= F_INORDER;
        }
        let mut ix = Index::new(with_meta);
        let mut live = 0;
        for i in 0..n {
            if flags[i] & F_DELETED != 0 {
                flags[i] |= F_FREED;
                ix.free.push(i as u32);
            } else {
                live += 1;
                if flags[i] & F_DIR != 0 {
                    ix.folders.insert((vol[i], frn[i]), i as u32);
                }
            }
        }
        ix.parent = parent;
        ix.name_off = name_off;
        ix.name_len = name_len;
        ix.flags = flags;
        ix.vol = vol;
        ix.frn = frn;
        ix.size = size;
        ix.mtime = mtime;
        ix.arena = arena;
        ix.vols = vols;
        ix.ids128 = ids128;
        ix.set_sorted(order);
        ix.live = live;
        ix.recount();
        Ok(ix)
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;

    #[test]
    fn incremental_checksum_equals_one_shot_for_any_split() {
        let data: Vec<u8> = (0..1000u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
            .collect();
        for len in [0usize, 1, 7, 8, 9, 63, 64, 65, 999, 1000] {
            let d = &data[..len];
            for step in [1usize, 3, 5, 8, 13, 100, 1000] {
                let mut ck = Ck::new();
                for c in d.chunks(step) {
                    ck.update(c);
                }
                assert_eq!(ck.finish(), checksum(d), "len {len} step {step}");
            }
        }
    }
}
