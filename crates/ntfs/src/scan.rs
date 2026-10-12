//! Volume enumeration and the streaming $MFT sweep.
//!
//! Pipeline: open `\\.\X:` (GENERIC_READ, buffered), `FSCTL_GET_NTFS_VOLUME_DATA`,
//! locate the $MFT extents from raw record 0's data runs, then a reader thread
//! pulls 512-record batches through the extent map while the calling thread
//! decodes them and feeds `sink`.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

use crate::ffi::{self, AlignedBuf, Handle};
use crate::record::{
    apply_fixup, decode_data_runs, parse_record, select_names_owned, u16_at, u32_at, u64_at, Parsed,
    RecordError, ATTR_ATTRIBUTE_LIST, ATTR_DATA, REC_NO_MASK,
};

/// Records per read (2 MiB with 1 KiB records).
const BATCH_RECORDS: usize = 2048;
const PROGRESS_EVERY_BATCHES: u64 = 8;
/// Decoder threads cap (the calling thread also runs the sink).
const MAX_WORKERS: usize = 8;
/// Extra buffers beyond 2 per worker so the reader never starves.
const READ_AHEAD: usize = 4;

/// One NTFS volume as reported by [`list_ntfs_volumes`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeInfo {
    pub letter: char,
    /// NTFS volume serial (64-bit when the volume could be queried, else the
    /// 32-bit `GetVolumeInformationW` serial zero-extended).
    pub serial: u64,
    pub bytes_per_cluster: u32,
    /// Bytes per MFT file record (1024 unless the volume could not be queried).
    pub record_size: u32,
}

/// One `(parent, name)` binding of an in-use base MFT record.
///
/// Hard links: a file with several `$FILE_NAME` attributes under different
/// parents (or different names) yields one `MftEntry` per distinct
/// `(parent, name)`, all sharing `frn`/`size`/`alloc`/`mtime`/`attrs`. The DOS
/// 8.3 alias is dropped whenever a Win32/POSIX name exists under the same parent.
/// The root directory (record 5) is emitted with name `"."` and
/// `parent_frn == frn`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MftEntry {
    /// File reference: 48-bit record number | sequence << 48 (matches USN FRNs).
    pub frn: u64,
    pub parent_frn: u64,
    pub name: String,
    pub is_dir: bool,
    /// Logical size of the unnamed data stream.
    pub size: u64,
    /// Allocated bytes (mapped clusters; resident data counts as 0).
    pub alloc: u64,
    /// Last-write FILETIME (100 ns ticks since 1601), from $STANDARD_INFORMATION.
    pub mtime: i64,
    /// FILE_ATTRIBUTE_* bits (DIRECTORY bit added for directories).
    pub attrs: u32,
}

#[derive(Debug, Clone, Default)]
pub struct ScanStats {
    pub mft_records: u64,
    pub entries: u64,
    pub in_use_base: u64,
    pub extension_records: u64,
    pub deleted_or_unused: u64,
    pub bad_magic: u64,
    pub bad_fixup: u64,
    pub nameless: u64,
    pub bytes_read: u64,
    pub elapsed: Duration,
    /// Decoder threads used.
    pub workers: u32,
    /// Reader thread: time inside `ReadFile` (disk-bound if ~= elapsed).
    pub read_time: Duration,
    /// Reader thread: time blocked waiting for a free buffer (decode-bound if large).
    pub reader_buf_wait: Duration,
    /// Sum over all decoder threads of time in fixup+parse+select (CPU time, not wall).
    pub decode_time: Duration,
    /// Calling thread: time in sink + merge + final parked resolution.
    pub sink_time: Duration,
    /// Calling thread: time idle waiting for decoded batches.
    pub main_wait: Duration,
}

#[derive(Debug)]
pub enum ScanError {
    /// Could not open the raw volume (usually: not elevated).
    Open { letter: char, source: io::Error },
    /// Volume data query failed (not NTFS?).
    VolumeData { letter: char, source: io::Error },
    /// Implausible geometry from the volume data.
    BadGeometry(String),
    /// Could not build the $MFT extent map.
    Locator(String),
    Read { offset: u64, source: io::Error },
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScanError::Open { letter, source } => {
                write!(f, "cannot open volume {letter}: ({source}); administrator rights required")
            }
            ScanError::VolumeData { letter, source } => {
                write!(f, "FSCTL_GET_NTFS_VOLUME_DATA failed on {letter}: ({source}); not NTFS?")
            }
            ScanError::BadGeometry(s) => write!(f, "bad volume geometry: {s}"),
            ScanError::Locator(s) => write!(f, "cannot locate $MFT: {s}"),
            ScanError::Read { offset, source } => write!(f, "read at byte {offset}: {source}"),
        }
    }
}

impl std::error::Error for ScanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ScanError::Open { source, .. }
            | ScanError::VolumeData { source, .. }
            | ScanError::Read { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Geometry {
    serial: u64,
    bytes_per_sector: u32,
    bytes_per_cluster: u64,
    record_size: usize,
    mft_valid_len: u64,
    mft_start_lcn: u64,
}

/// {StartVCN, Clusters, LCN}; `lcn < 0` means sparse.
type Run = (u64, u64, i64);

/// Parse a 96-byte NTFS_VOLUME_DATA_BUFFER.
fn parse_volume_data(buf: &[u8]) -> Geometry {
    Geometry {
        serial: u64_at(buf, 0),
        bytes_per_sector: u32_at(buf, 40),
        // BytesPerCluster is a ULONG at +44 (reading u64 swallows the next field).
        bytes_per_cluster: u32_at(buf, 44) as u64,
        record_size: u32_at(buf, 48) as usize,
        mft_valid_len: u64_at(buf, 56),
        mft_start_lcn: u64_at(buf, 64),
    }
}

fn query_volume_data(h: &Handle) -> io::Result<Geometry> {
    let mut buf = [0u8; 96];
    h.ioctl(ffi::FSCTL_GET_NTFS_VOLUME_DATA, &[], &mut buf)?;
    Ok(parse_volume_data(&buf))
}

/// All fixed/removable drives whose filesystem is NTFS. Works without
/// elevation (geometry falls back to `GetDiskFreeSpaceW` if the volume
/// cannot be opened for query).
pub fn list_ntfs_volumes() -> Vec<VolumeInfo> {
    let mut out = Vec::new();
    for letter in ffi::logical_drives() {
        let Some((fs, serial32)) = ffi::volume_fs_and_serial(letter) else { continue };
        if !fs.eq_ignore_ascii_case("NTFS") {
            continue;
        }
        let mut info = VolumeInfo {
            letter,
            serial: serial32 as u64,
            bytes_per_cluster: ffi::disk_geometry(letter).map_or(4096, |g| g.1),
            record_size: 1024,
        };
        // Access 0 = query-only handle; usually allowed for non-admin callers.
        if let Ok(h) = Handle::open_volume(letter, 0, 0) {
            if let Ok(g) = query_volume_data(&h) {
                info.serial = g.serial;
                if g.bytes_per_cluster != 0 {
                    info.bytes_per_cluster = g.bytes_per_cluster as u32;
                }
                if g.record_size != 0 {
                    info.record_size = g.record_size as u32;
                }
            }
        }
        out.push(info);
    }
    out
}

/// Open a volume for USN journal ioctls: GENERIC_READ **with**
/// `FILE_FLAG_NO_BUFFERING` (what Everything does for its monitor handles).
pub fn open_volume_for_usn(letter: char) -> Result<Handle, ScanError> {
    Handle::open_volume(letter, ffi::GENERIC_READ, ffi::FILE_FLAG_NO_BUFFERING)
        .map_err(|source| ScanError::Open { letter, source })
}

/// Open a volume for the $MFT sweep: GENERIC_READ, **no** NO_BUFFERING.
fn open_volume_for_scan(letter: char) -> Result<Handle, ScanError> {
    Handle::open_volume(letter, ffi::GENERIC_READ, 0)
        .map_err(|source| ScanError::Open { letter, source })
}

fn validate_geometry(g: &Geometry) -> Result<(), ScanError> {
    let bad = |s: &str| Err(ScanError::BadGeometry(format!("{s}: {g:?}")));
    if !(512..=65536).contains(&g.record_size) || !g.record_size.is_power_of_two() {
        return bad("record size");
    }
    if g.bytes_per_cluster < 512 || g.bytes_per_cluster > (1 << 26) || !g.bytes_per_cluster.is_power_of_two() {
        return bad("cluster size");
    }
    if g.mft_valid_len == 0 || g.mft_start_lcn == 0 {
        return bad("MFT length/start");
    }
    Ok(())
}

/// Locator: read the first 16 MFT records raw at MftStartLcn x cluster, take
/// record 0's unnamed non-resident $DATA runs, plus runs from extension
/// records named in record 0's resident $ATTRIBUTE_LIST (type 0x80, LowestVCN>0)
/// that lie within the first 16 slots. (Taking every $DATA in the first 16
/// would swallow $BadClus, which is whole-volume sparse.)
fn extents_from_record0(vol: &Handle, g: &Geometry) -> Result<Vec<Run>, ScanError> {
    let rs = g.record_size;
    let sector = (g.bytes_per_sector as usize).max(512);
    let buf_size = (16 * rs).div_ceil(sector) * sector;
    let mut buf = AlignedBuf::new(buf_size);
    let offset = g.mft_start_lcn * g.bytes_per_cluster;
    vol.read_at(&mut buf.as_mut_slice()[..buf_size], offset)
        .map_err(|source| ScanError::Read { offset, source })?;
    let data = buf.as_mut_slice();
    for s in 0..16 {
        let rec = &mut data[s * rs..(s + 1) * rs];
        if &rec[0..4] == b"FILE" {
            let _ = apply_fixup(rec);
        }
    }
    let mut runs = Vec::new();
    let targets = {
        let r0 = &data[..rs];
        if &r0[0..4] != b"FILE" {
            return Err(ScanError::Locator("record 0 has no FILE magic".into()));
        }
        collect_runs(r0, &mut runs);
        data_ext_targets(r0)
    };
    for t in targets {
        if (t as usize) < 16 {
            let rec = &data[t as usize * rs..(t as usize + 1) * rs];
            if &rec[0..4] == b"FILE" {
                collect_runs(rec, &mut runs);
            }
        }
    }
    runs.sort_by_key(|r| r.0);
    if runs.is_empty() {
        return Err(ScanError::Locator("no $MFT data runs in record 0".into()));
    }
    Ok(runs)
}

/// Iterate attributes of a (fixed-up) record: (type, offset, length).
fn attrs_of(rec: &[u8]) -> Vec<(u32, usize, usize)> {
    let mut out = Vec::new();
    let mut off = u16_at(rec, 0x14) as usize;
    let mut used = u32_at(rec, 0x18) as usize;
    if used == 0 || used > rec.len() {
        used = rec.len();
    }
    while off + 0x18 <= used {
        let ty = u32_at(rec, off);
        if ty == 0xFFFF_FFFF || ty == 0 {
            break;
        }
        let len = u32_at(rec, off + 4) as usize;
        if len < 0x18 || off + len > used {
            break;
        }
        out.push((ty, off, len));
        off += len;
    }
    out
}

fn data_ext_targets(rec: &[u8]) -> Vec<u32> {
    let mut targets = Vec::new();
    for (ty, off, len) in attrs_of(rec) {
        if ty != ATTR_ATTRIBUTE_LIST || rec[off + 8] != 0 {
            continue;
        }
        let val_off = off + u16_at(rec, off + 0x14) as usize;
        let val_len = u32_at(rec, off + 0x10) as usize;
        let end = (val_off + val_len).min(off + len);
        let mut pos = val_off;
        while pos + 0x18 <= end {
            let etype = u32_at(rec, pos);
            let elen = u16_at(rec, pos + 4) as usize;
            if etype == 0xFFFF_FFFF || etype == 0 || elen == 0 {
                break;
            }
            if etype == ATTR_DATA {
                let lowest = u64_at(rec, pos + 8);
                let ext = (u64_at(rec, pos + 0x10) & REC_NO_MASK) as u32;
                if lowest > 0 && ext != 0 {
                    targets.push(ext);
                }
            }
            pos += elen;
        }
    }
    targets
}

fn collect_runs(rec: &[u8], out: &mut Vec<Run>) {
    for (ty, off, len) in attrs_of(rec) {
        if ty == ATTR_DATA && rec[off + 8] == 1 && rec[off + 9] == 0 {
            let mut vcn = u64_at(rec, off + 0x10);
            let runs_off = off + u16_at(rec, off + 0x20) as usize;
            for (clusters, lcn) in decode_data_runs(rec, runs_off, off + len) {
                if let Some(lcn) = lcn {
                    out.push((vcn, clusters, lcn));
                }
                vcn += clusters;
            }
        }
    }
}

/// record number -> (byte offset on volume, records left in this run, sparse
/// clusters to skip). The `(vcn - start_vcn)` term matters.
fn locate(extents: &[Run], recno: u64, cluster: u64, rec_size: usize) -> (Option<u64>, u64, u64) {
    let byte = recno * rec_size as u64;
    let vcn = byte / cluster;
    let in_cluster = byte % cluster;
    for &(start, clusters, lcn) in extents {
        if start <= vcn && vcn < start + clusters {
            if lcn < 0 {
                return (None, 0, start + clusters - vcn);
            }
            let offset = (lcn as u64 + (vcn - start)) * cluster + in_cluster;
            let remaining = (start + clusters - vcn) * cluster - in_cluster;
            return (Some(offset), remaining / rec_size as u64, 0);
        }
    }
    (None, 0, 0)
}

struct Batch {
    buf: AlignedBuf,
    recno: u64,
    skip: usize,
    n: usize,
    got: usize,
}

enum Msg {
    Done { n: usize, decoded: Box<Decoded> },
    Failed(ScanError),
}

/// Result of decoding one batch of records on a worker thread.
#[derive(Default)]
pub(crate) struct Decoded {
    /// Entries of plain (no $ATTRIBUTE_LIST) in-use base records.
    pub entries: Vec<MftEntry>,
    /// Bases with an $ATTRIBUTE_LIST: resolved globally after the sweep.
    pub parked: Vec<Parsed>,
    /// Extension records whose base was not seen (earlier) inside this batch.
    pub orphans: Vec<Parsed>,
    /// Counters only (no timings).
    pub stats: ScanStats,
}

fn build_entries(p: Parsed, stats: &mut ScanStats, out: &mut impl FnMut(MftEntry)) {
    let Parsed { recno, seq, is_dir, names, std, size, alloc, .. } = p;
    let frn = (recno as u64) | ((seq as u64) << 48);
    let names = select_names_owned(names);
    if names.is_empty() {
        stats.nameless += 1;
        return;
    }
    let (mtime, mut attrs) = std.unwrap_or((0, 0));
    if is_dir {
        attrs |= 0x10;
    }
    for n in names {
        out(MftEntry {
            frn,
            parent_frn: n.parent_ref,
            name: n.name,
            is_dir,
            size: size.unwrap_or(0),
            alloc,
            mtime: mtime as i64,
            attrs,
        });
        stats.entries += 1;
    }
}

/// Decode `n` records of `rs` bytes starting at `data[skip..]` (record numbers
/// `first_recno..`), with `got` valid bytes counted from the start of `data`.
/// A pure function of its input: this is what the workers (and the offline
/// benchmark) run. An extension record is merged into a base with no
/// $ATTRIBUTE_LIST when both fall in the same slice (extension first); every
/// other extension record is returned as an orphan, every base with an
/// $ATTRIBUTE_LIST as parked, for global resolution after the sweep.
pub(crate) fn decode_slice(
    data: &mut [u8],
    first_recno: u64,
    skip: usize,
    n: usize,
    got: usize,
    rs: usize,
    cluster: u64,
) -> Decoded {
    let mut out = Decoded { entries: Vec::with_capacity(n), ..Default::default() };
    let mut local_orphans: HashMap<u32, Vec<Parsed>> = HashMap::new();
    for k in 0..n {
        let base = skip + k * rs;
        if base + rs > skip + got || base + rs > data.len() {
            break; // short read: ignore trailing partial record
        }
        let recno = (first_recno + k as u64) as u32;
        match parse_record(&mut data[base..base + rs], recno, cluster) {
            Err(RecordError::BadMagic) => out.stats.bad_magic += 1,
            Err(RecordError::NotInUse) => out.stats.deleted_or_unused += 1,
            Err(_) => out.stats.bad_fixup += 1,
            Ok(p) if p.base_ref != 0 => {
                out.stats.extension_records += 1;
                local_orphans.entry(p.base_ref).or_default().push(p);
            }
            Ok(mut p) => {
                out.stats.in_use_base += 1;
                if !local_orphans.is_empty() {
                    if let Some(v) = local_orphans.remove(&recno) {
                        for e in v {
                            p.merge(e);
                        }
                    }
                }
                if p.has_attr_list {
                    out.parked.push(p);
                } else {
                    build_entries(p, &mut out.stats, &mut |e| out.entries.push(e));
                }
            }
        }
    }
    out.orphans = local_orphans.into_values().flatten().collect();
    out
}

fn add_counts(dst: &mut ScanStats, s: &ScanStats) {
    dst.in_use_base += s.in_use_base;
    dst.extension_records += s.extension_records;
    dst.deleted_or_unused += s.deleted_or_unused;
    dst.bad_magic += s.bad_magic;
    dst.bad_fixup += s.bad_fixup;
    dst.nameless += s.nameless;
    dst.entries += s.entries;
}

/// Final global pass: attach every orphaned extension record to its parked
/// base (in record-number order, like the old serial sweep), then emit the
/// parked bases in record-number order.
fn resolve_parked(
    parked: Vec<Parsed>,
    mut orphans: Vec<Parsed>,
    stats: &mut ScanStats,
    sink: &mut impl FnMut(MftEntry),
) {
    let mut map: HashMap<u32, Parsed> = parked.into_iter().map(|p| (p.recno, p)).collect();
    orphans.sort_unstable_by_key(|o| (o.base_ref, o.recno));
    for o in orphans {
        if let Some(b) = map.get_mut(&o.base_ref) {
            b.merge(o);
        }
    }
    let mut keys: Vec<u32> = map.keys().copied().collect();
    keys.sort_unstable();
    for k in keys {
        if let Some(p) = map.remove(&k) {
            build_entries(p, stats, sink);
        }
    }
}

/// Scan `letter:` and stream one [`MftEntry`] per (parent, name) of every
/// in-use base record to `sink` (always called on the calling thread).
/// Needs an elevated process. `progress` is called as `(records_done,
/// records_total)` periodically and once at the end.
///
/// Records are decoded on a pool of worker threads, so entries arrive in
/// batch-completion order (not MFT order). Entries for records whose
/// attributes live in extension records are delivered at the very end of the
/// sweep (after all plain records).
pub fn scan_volume(
    letter: char,
    sink: &mut impl FnMut(MftEntry),
    progress: &mut impl FnMut(u64, u64),
) -> Result<ScanStats, ScanError> {
    let t0 = Instant::now();
    let letter = letter.to_ascii_uppercase();
    let vol = open_volume_for_scan(letter)?;
    let g = query_volume_data(&vol).map_err(|source| ScanError::VolumeData { letter, source })?;
    validate_geometry(&g)?;
    let rs = g.record_size;
    let cluster = g.bytes_per_cluster;
    let sector = (g.bytes_per_sector as usize).max(512);
    let total = g.mft_valid_len / rs as u64;
    let extents = extents_from_record0(&vol, &g)?;

    let workers = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, MAX_WORKERS);
    let nbufs = workers * 2 + READ_AHEAD;
    let buf_size = BATCH_RECORDS * rs + sector;
    let (out_tx, out_rx) = mpsc::channel::<Msg>();
    let (batch_tx, batch_rx) = mpsc::sync_channel::<Batch>(nbufs);
    let batch_rx = Mutex::new(batch_rx);
    let (free_tx, free_rx) = mpsc::channel::<AlignedBuf>();
    for _ in 0..nbufs {
        let _ = free_tx.send(AlignedBuf::new(buf_size));
    }

    let mut stats = ScanStats { mft_records: total, workers: workers as u32, ..Default::default() };
    let mut failure: Option<ScanError> = None;
    let mut parked: Vec<Parsed> = Vec::new();
    let mut orphans: Vec<Parsed> = Vec::new();

    std::thread::scope(|s| {
        // Reader: sequential extent walk, one synchronous buffered read per batch.
        let reader = {
            let (vol_ref, extents, out_tx) = (&vol, &extents, out_tx.clone());
            s.spawn(move || {
                let (mut read_time, mut buf_wait, mut bytes) = (Duration::ZERO, Duration::ZERO, 0u64);
                let mut recno: u64 = 0;
                while recno < total {
                    let (offset, run_remaining, sparse) = locate(extents, recno, cluster, rs);
                    if sparse > 0 {
                        recno += (sparse * cluster / rs as u64).min(total - recno);
                        continue;
                    }
                    let Some(offset) = offset else { break };
                    let n = (BATCH_RECORDS as u64).min(run_remaining).min(total - recno) as usize;
                    if n == 0 {
                        break;
                    }
                    let aligned = offset / sector as u64 * sector as u64;
                    let skip = (offset - aligned) as usize;
                    let read_len = (skip + n * rs).div_ceil(sector) * sector;
                    let tw = Instant::now();
                    let Ok(mut buf) = free_rx.recv() else { break };
                    buf_wait += tw.elapsed();
                    let tr = Instant::now();
                    let res = vol_ref.read_at(&mut buf.as_mut_slice()[..read_len], aligned);
                    read_time += tr.elapsed();
                    match res {
                        Ok(got) => {
                            bytes += got as u64;
                            if batch_tx.send(Batch { buf, recno, skip, n, got }).is_err() {
                                break;
                            }
                        }
                        Err(source) => {
                            let _ = out_tx.send(Msg::Failed(ScanError::Read { offset: aligned, source }));
                            break;
                        }
                    }
                    recno += n as u64;
                }
                (read_time, buf_wait, bytes)
            })
        };

        // Decoders: pull batches, decode without touching the sink.
        let decoders: Vec<_> = (0..workers)
            .map(|_| {
                let (out_tx, free_tx, batch_rx) = (out_tx.clone(), free_tx.clone(), &batch_rx);
                s.spawn(move || {
                    let mut decode_time = Duration::ZERO;
                    loop {
                        let Ok(mut b) = batch_rx.lock().unwrap().recv() else { break };
                        let t = Instant::now();
                        let decoded =
                            decode_slice(b.buf.as_mut_slice(), b.recno, b.skip, b.n, b.got, rs, cluster);
                        decode_time += t.elapsed();
                        let _ = free_tx.send(b.buf);
                        if out_tx.send(Msg::Done { n: b.n, decoded: Box::new(decoded) }).is_err() {
                            break;
                        }
                    }
                    decode_time
                })
            })
            .collect();
        drop(out_tx);
        drop(free_tx);

        let (mut done_recs, mut batches) = (0u64, 0u64);
        loop {
            let tw = Instant::now();
            let Ok(msg) = out_rx.recv() else { break };
            stats.main_wait += tw.elapsed();
            match msg {
                Msg::Failed(e) => {
                    failure = Some(e);
                    break;
                }
                Msg::Done { n, decoded } => {
                    let t = Instant::now();
                    add_counts(&mut stats, &decoded.stats);
                    parked.extend(decoded.parked);
                    orphans.extend(decoded.orphans);
                    for e in decoded.entries {
                        sink(e);
                    }
                    stats.sink_time += t.elapsed();
                    done_recs += n as u64;
                    batches += 1;
                    if batches.is_multiple_of(PROGRESS_EVERY_BATCHES) {
                        progress(done_recs.min(total), total);
                    }
                }
            }
        }
        // Early break: dropping the receiver makes the workers' sends fail so
        // they (and, via the closed channels, the reader) wind down.
        drop(out_rx);
        let (rt, bw, bytes) = reader.join().unwrap_or_default();
        stats.read_time = rt;
        stats.reader_buf_wait = bw;
        stats.bytes_read = bytes;
        for d in decoders {
            stats.decode_time += d.join().unwrap_or_default();
        }
    });

    if let Some(e) = failure {
        return Err(e);
    }
    let t = Instant::now();
    resolve_parked(parked, orphans, &mut stats, sink);
    stats.sink_time += t.elapsed();
    progress(total, total);
    stats.elapsed = t0.elapsed();
    Ok(stats)
}

/// Offline benchmark hook (no disk I/O): decode `batches` copies of `template`
/// (a sealed run of `rs`-byte records) on `threads` workers using the same
/// decode/merge path as [`scan_volume`]. Returns `(records, entries, elapsed)`.
#[cfg(feature = "testutil")]
#[doc(hidden)]
pub fn bench_decode(template: &[u8], rs: usize, batches: usize, threads: usize) -> (u64, u64, Duration) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let per = template.len() / rs;
    let next = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel::<Decoded>();
    let t0 = Instant::now();
    let (mut entries, mut records) = (0u64, 0u64);
    let (mut parked, mut orphans) = (Vec::new(), Vec::new());
    let mut stats = ScanStats::default();
    std::thread::scope(|s| {
        for _ in 0..threads.max(1) {
            let tx = tx.clone();
            let next = &next;
            s.spawn(move || {
                let mut buf = template.to_vec();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= batches {
                        break;
                    }
                    buf.copy_from_slice(template);
                    let d = decode_slice(&mut buf, (i * per) as u64, 0, per, per * rs, rs, 4096);
                    if tx.send(d).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        for d in rx {
            records += per as u64;
            add_counts(&mut stats, &d.stats);
            parked.extend(d.parked);
            orphans.extend(d.orphans);
            for e in d.entries {
                entries += 1;
                std::hint::black_box(&e);
            }
        }
    });
    resolve_parked(parked, orphans, &mut stats, &mut |e| {
        entries += 1;
        std::hint::black_box(&e);
    });
    (records, entries, t0.elapsed())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_data_layout() {
        let mut b = [0u8; 96];
        b[0..8].copy_from_slice(&0xDEAD_BEEF_u64.to_le_bytes());
        b[40..44].copy_from_slice(&512u32.to_le_bytes());
        b[44..48].copy_from_slice(&4096u32.to_le_bytes());
        b[48..52].copy_from_slice(&1024u32.to_le_bytes());
        b[52..56].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // ClustersPerFRS next field
        b[56..64].copy_from_slice(&(1u64 << 30).to_le_bytes());
        b[64..72].copy_from_slice(&786_432u64.to_le_bytes());
        let g = parse_volume_data(&b);
        assert_eq!(g.serial, 0xDEAD_BEEF);
        assert_eq!(g.bytes_per_cluster, 4096);
        assert_eq!(g.record_size, 1024);
        assert_eq!(g.mft_valid_len, 1 << 30);
        assert_eq!(g.mft_start_lcn, 786_432);
        assert!(validate_geometry(&g).is_ok());
    }

    #[test]
    fn locate_uses_vcn_offset_and_sparse() {
        // 2 extents: vcn 0..100 @ lcn 1000; vcn 100..110 sparse
        let ex: Vec<Run> = vec![(0, 100, 1000), (100, 10, -1)];
        let (off, rem, sp) = locate(&ex, 8, 4096, 1024); // vcn 2, in-cluster 0
        assert_eq!(off, Some((1000 + 2) * 4096));
        assert_eq!(rem, (98 * 4096) / 1024);
        assert_eq!(sp, 0);
        let (off, _, sp) = locate(&ex, 400, 4096, 1024); // vcn 100 -> sparse
        assert_eq!(off, None);
        assert_eq!(sp, 10);
        assert_eq!(locate(&ex, 10_000, 4096, 1024), (None, 0, 0));
    }

    #[test]
    fn bad_geometry_rejected() {
        let g = Geometry {
            serial: 1,
            bytes_per_sector: 512,
            bytes_per_cluster: 4096,
            record_size: 1000,
            mft_valid_len: 1,
            mft_start_lcn: 1,
        };
        assert!(validate_geometry(&g).is_err());
    }

    #[test]
    fn parked_bases_merge_extensions_found_before_or_after() {
        use crate::record::NameAttr;
        let mut out = Vec::new();
        let mut sink = |e: MftEntry| out.push(e);
        let base = |recno: u32| Parsed {
            recno,
            seq: 2,
            has_attr_list: true,
            names: vec![NameAttr { parent_ref: 5, namespace: 1, name: format!("f{recno}") }],
            ..Default::default()
        };
        let ext = |recno: u32, b: u32| Parsed {
            recno,
            base_ref: b,
            size: Some(1000),
            alloc: 4096,
            ..Default::default()
        };
        let mut stats = ScanStats::default();
        resolve_parked(
            vec![base(20), base(10)],
            vec![ext(60, 20), ext(50, 10), ext(70, 999)],
            &mut stats,
            &mut sink,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "f10");
        assert_eq!((out[0].size, out[0].alloc), (1000, 4096));
        assert_eq!(out[1].frn, 20 | (2u64 << 48));
        assert_eq!(out[1].size, 1000);
    }

    /// Splitting the same record stream into different batch sizes must give
    /// the same entry set (extensions straddling batches resolve globally).
    #[test]
    fn batch_split_does_not_change_result() {
        use crate::record::testutil::Rb;
        const CL: u64 = 4096;
        let recs: Vec<Vec<u8>> = vec![
            Rb::new(true, false).name(5, "zero", 1).seal(), // 0
            Rb::new(true, false).name(5, "big", 1).attr_list().seal(), // 1
            Rb::new(true, false).base(3 | (7 << 48)).data_nonres(0, 777, 2).seal(), // 2: ext of 3
            Rb::new(true, false).name(5, "later", 1).attr_list().seal(), // 3
            Rb::new(false, false).name(5, "gone", 1).seal(), // 4
            vec![0u8; 1024], // 5
            Rb::new(true, false).base(1 | (7 << 48)).data_nonres(0, 555, 3).seal(), // 6: ext of 1
            Rb::new(true, true).name(5, "d", 1).name(5, "D~1", 2).seal(), // 7
        ];
        let flat: Vec<u8> = recs.concat();
        let run = |per: usize| {
            let mut got: Vec<MftEntry> = Vec::new();
            let (mut parked, mut orphans) = (Vec::new(), Vec::new());
            let mut st = ScanStats::default();
            let mut data = flat.clone();
            for (ci, chunk) in data.chunks_mut(per * 1024).enumerate() {
                let n = chunk.len() / 1024;
                let d = decode_slice(chunk, (ci * per) as u64, 0, n, n * 1024, 1024, CL);
                add_counts(&mut st, &d.stats);
                got.extend(d.entries);
                parked.extend(d.parked);
                orphans.extend(d.orphans);
            }
            resolve_parked(parked, orphans, &mut st, &mut |e| got.push(e));
            got.sort_by_key(|e| (e.frn & REC_NO_MASK, e.name.clone()));
            (got, st.bad_magic, st.deleted_or_unused, st.extension_records)
        };
        let whole = run(8);
        for per in [1, 2, 3, 5] {
            assert_eq!(run(per), whole, "per={per}");
        }
        let (e, bad, del, ext) = whole;
        assert_eq!((bad, del, ext), (1, 1, 2));
        let by = |n: &str| e.iter().find(|x| x.name == n).unwrap();
        assert_eq!(by("big").size, 555);
        assert_eq!(by("later").size, 777);
        assert_eq!(e.len(), 4); // zero, big, later, d (DOS alias dropped)
    }
}
