//! Snapshot: `WSIX` v1, little endian, checksummed.
//!
//! ```text
//! magic "WSIX" | version u32 | flags u32 (bit0 = size/mtime columns) | nvols u32 | nrec u32 | norder u32
//! per volume: letter u8 | serial u64 | journal_id u64 | next_usn i64 | root u32
//! parent u32[nrec] | vol u8[nrec] | flags u8[nrec] | frn u64[nrec] | (size u64[nrec] | mtime i64[nrec])
//! names_len u64 | front-coded names in STORAGE order: per record varint(shared) varint(suffix_len) bytes
//! order u32[norder]   (name sort array, deleted/stale/pending already folded in)
//! checksum u64 (over everything before it)
//! ```
//! Deleted slots are stored (empty name, DELETED flag) so record indices stay stable.
//! The FRN folder map, child links and hash table are rebuilt on load / lazily.

#![allow(clippy::chunks_exact_to_as_chunks)]

use crate::store::*;
use std::fmt;
use std::io;
use std::path::Path;

const MAGIC: &[u8; 4] = b"WSIX";
const VERSION: u32 = 1;

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
        let end = self.p.checked_add(n).filter(|&e| e <= self.d.len()).ok_or(SnapshotError::Corrupt("truncated"))?;
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
        Ok(self.take(n.checked_mul(4).ok_or(SnapshotError::Corrupt("len"))?)?
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect())
    }
    fn u64s(&mut self, n: usize) -> Result<Vec<u64>, SnapshotError> {
        Ok(self.take(n.checked_mul(8).ok_or(SnapshotError::Corrupt("len"))?)?
            .chunks_exact(8)
            .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
            .collect())
    }
}

impl Index {
    /// Serialize to bytes (folds pending changes into the saved order without mutating).
    pub fn to_bytes(&self) -> Vec<u8> {
        let order = self.clean_order();
        let n = self.parent.len();
        let mut b = Vec::with_capacity(64 + n * 24 + self.arena.len() + order.len() * 4);
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
        }
        for &p in &self.parent {
            b.extend_from_slice(&p.to_le_bytes());
        }
        b.extend_from_slice(&self.vol);
        b.extend(self.flags.iter().map(|f| f & F_PERSIST));
        for &f in &self.frn {
            b.extend_from_slice(&f.to_le_bytes());
        }
        if self.with_meta {
            for &s in &self.size {
                b.extend_from_slice(&s.to_le_bytes());
            }
            for &m in &self.mtime {
                b.extend_from_slice(&m.to_le_bytes());
            }
        }
        let mut names = Vec::with_capacity(self.arena.len());
        let mut prev: &[u8] = &[];
        for r in 0..n as u32 {
            let cur = self.name_bytes(r);
            let shared = prev.iter().zip(cur).take_while(|(a, b)| a == b).count();
            put_varint(&mut names, shared as u64);
            put_varint(&mut names, (cur.len() - shared) as u64);
            names.extend_from_slice(&cur[shared..]);
            prev = cur;
        }
        b.extend_from_slice(&(names.len() as u64).to_le_bytes());
        b.extend_from_slice(&names);
        for &o in order.iter() {
            b.extend_from_slice(&o.to_le_bytes());
        }
        let ck = checksum(&b);
        b.extend_from_slice(&ck.to_le_bytes());
        b
    }

    /// Write `<path>.tmp`, then rename over `path`.
    pub fn save(&self, path: &Path) -> Result<(), SnapshotError> {
        let bytes = self.to_bytes();
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = std::path::PathBuf::from(tmp);
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&bytes)?;
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
        if r.u32()? != VERSION {
            return Err(c("version"));
        }
        let fl = r.u32()?;
        let with_meta = fl & 1 != 0;
        let nvols = r.u32()? as usize;
        let n = r.u32()? as usize;
        let norder = r.u32()? as usize;
        let per = 4 + 1 + 1 + 8 + if with_meta { 16 } else { 0 };
        if nvols > 256 || norder > n || n >= (u32::MAX - 2) as usize || n.saturating_mul(per) > body.len() {
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
            vols.push(VolumeMeta { letter, serial, journal_id, next_usn, root });
        }
        let parent = r.u32s(n)?;
        let vol = r.take(n)?.to_vec();
        let mut flags = r.take(n)?.to_vec();
        let frn = r.u64s(n)?;
        let (size, mtime) = if with_meta {
            (r.u64s(n)?, r.u64s(n)?.into_iter().map(|x| x as i64).collect())
        } else {
            (Vec::new(), Vec::new())
        };
        let names_len = r.u64()? as usize;
        let mut nr = Rd { d: r.take(names_len)?, p: 0 };
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
                if p as usize >= n || flags[p as usize] & (F_DIR | F_DELETED) != F_DIR || p as usize == i {
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
            if ou >= n || flags[ou] & (F_DELETED | F_ROOT | F_STUB) != 0 || flags[ou] & F_INORDER != 0 {
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
        ix.rank = ix.compute_rank(&order);
        ix.order = order;
        ix.live = live;
        Ok(ix)
    }
}
