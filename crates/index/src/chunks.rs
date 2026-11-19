//! The name-sorted order as a chunked array (Everything's primitive: every sorted array there
//! is a list of 64 KB chunks with per-chunk counts, so an insert moves one chunk, not the
//! whole array).
//!
//! `rank[slot]` holds a *code* `block id << OFF_BITS | offset in block`, not a position: a
//! change rewrites the codes of the blocks it touched only. The dense position (what the
//! match bitsets index) is `start[block id] + offset`, with `start` recomputed per refresh in
//! O(blocks). Blocks hold `FILL` entries when built and split in two at `MAX_BLOCK`.

use crate::store::NONE;

#[cfg(not(test))]
pub(crate) const OFF_BITS: u32 = 13;
/// Tiny blocks under test so splits, empties and multi-block searches are exercised.
#[cfg(test)]
pub(crate) const OFF_BITS: u32 = 3;
pub(crate) const MAX_BLOCK: usize = 1 << OFF_BITS;
const OFF_MASK: u32 = (1 << OFF_BITS) - 1;
const FILL: usize = MAX_BLOCK / 2;

/// A place in a [`Chunks`]: the `k`-th block in sequence, entry `off` in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pos {
    k: usize,
    off: usize,
}

#[derive(Default, Clone)]
pub(crate) struct Chunks {
    /// Entries per block id (an unused id holds an empty block).
    blocks: Vec<Vec<u32>>,
    /// Non-empty block ids in sort order.
    seq: Vec<u32>,
    /// Dense position of each block's first entry, by block id.
    start: Vec<u32>,
    /// Ids of blocks that emptied, for reuse.
    spare: Vec<u32>,
    len: usize,
}

impl Chunks {
    /// Build from a sorted list and write every entry's code into `rank` (sized to `slots`).
    pub(crate) fn from_sorted(v: &[u32], rank: &mut Vec<u32>, slots: usize) -> Chunks {
        rank.clear();
        rank.resize(slots, NONE);
        let mut c = Chunks::default();
        for part in v.chunks(FILL) {
            let id = c.blocks.len() as u32;
            for (k, &r) in part.iter().enumerate() {
                rank[r as usize] = (id << OFF_BITS) | k as u32;
            }
            c.blocks.push(part.to_vec());
            c.seq.push(id);
        }
        c.len = v.len();
        c.restart();
        c
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// Dense position of an entry from its code.
    #[inline]
    pub(crate) fn dense(&self, code: u32) -> u32 {
        self.start[(code >> OFF_BITS) as usize] + (code & OFF_MASK)
    }

    /// The entry at dense position `pos`.
    pub(crate) fn at(&self, pos: usize) -> Option<u32> {
        if pos >= self.len {
            return None;
        }
        // Last block in sequence whose start is <= pos.
        let k = self
            .seq
            .partition_point(|&b| self.start[b as usize] as usize <= pos)
            - 1;
        let b = self.seq[k] as usize;
        self.blocks[b].get(pos - self.start[b] as usize).copied()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        self.seq
            .iter()
            .flat_map(move |&b| self.blocks[b as usize].iter().copied())
    }

    pub(crate) fn to_vec(&self) -> Vec<u32> {
        let mut v = Vec::with_capacity(self.len);
        for &b in &self.seq {
            v.extend_from_slice(&self.blocks[b as usize]);
        }
        v
    }

    pub(crate) fn mem_bytes(&self) -> usize {
        self.blocks.iter().map(|b| b.capacity() * 4).sum::<usize>()
            + (self.blocks.capacity() * 3 + self.seq.capacity() + self.start.capacity()) * 4
    }

    /// Drop the entries `leaving(r)` says go from the blocks holding `codes`; returns the
    /// touched block ids. Emptied blocks leave the sequence.
    pub(crate) fn remove_where(
        &mut self,
        codes: &[u32],
        mut leaving: impl FnMut(u32) -> bool,
        touched: &mut Vec<u32>,
    ) {
        let mut ids: Vec<u32> = codes.iter().map(|&c| c >> OFF_BITS).collect();
        ids.sort_unstable();
        ids.dedup();
        let mut emptied = false;
        for &b in &ids {
            let blk = &mut self.blocks[b as usize];
            let before = blk.len();
            blk.retain(|&r| !leaving(r));
            self.len -= before - blk.len();
            if blk.is_empty() {
                blk.shrink_to_fit();
                emptied = true;
            }
            touched.push(b);
        }
        if emptied {
            let blocks = &self.blocks;
            let spare = &mut self.spare;
            self.seq.retain(|&b| {
                let keep = !blocks[b as usize].is_empty();
                if !keep {
                    spare.push(b);
                }
                keep
            });
        }
    }

    /// Insert `a`, sorted by `less(x, a)` = "x sorts before a"; returns the touched block ids
    /// (one, or two after a split).
    pub(crate) fn insert(&mut self, a: u32, less: impl Fn(u32) -> bool, touched: &mut Vec<u32>) {
        self.len += 1;
        if self.seq.is_empty() {
            let id = self.new_block(vec![a]);
            self.seq.push(id);
            touched.push(id);
            return;
        }
        // First block whose last entry does not sort before `a` (else the last block).
        let k = self
            .seq
            .partition_point(|&b| less(*self.blocks[b as usize].last().expect("non-empty")))
            .min(self.seq.len() - 1);
        let b = self.seq[k];
        let blk = &mut self.blocks[b as usize];
        let at = blk.partition_point(|&x| less(x));
        blk.insert(at, a);
        touched.push(b);
        if blk.len() >= MAX_BLOCK {
            let tail = blk.split_off(blk.len() / 2);
            let id = self.new_block(tail);
            self.seq.insert(k + 1, id);
            touched.push(id);
        }
    }

    /// Build from a sorted list, without codes (a lookup structure, not the name order).
    pub(crate) fn from_sorted_plain(v: &[u32]) -> Chunks {
        let mut c = Chunks::default();
        for part in v.chunks(FILL) {
            c.blocks.push(part.to_vec());
            c.seq.push((c.blocks.len() - 1) as u32);
        }
        c.len = v.len();
        c.restart();
        c
    }

    /// The first entry `x` with `!less(x)` (entries are sorted by `less`).
    pub(crate) fn lower_bound(&self, less: impl Fn(u32) -> bool) -> Pos {
        let k = self
            .seq
            .partition_point(|&b| less(*self.blocks[b as usize].last().expect("non-empty")));
        let off = match self.seq.get(k) {
            Some(&b) => self.blocks[b as usize].partition_point(|&x| less(x)),
            None => 0,
        };
        Pos { k, off }
    }

    pub(crate) fn get(&self, p: Pos) -> Option<u32> {
        let &b = self.seq.get(p.k)?;
        self.blocks[b as usize].get(p.off).copied()
    }

    pub(crate) fn advance(&self, p: &mut Pos) {
        p.off += 1;
        if let Some(&b) = self.seq.get(p.k) {
            if p.off >= self.blocks[b as usize].len() {
                p.k += 1;
                p.off = 0;
            }
        }
    }

    /// Remove the entry at `p` (which must exist).
    pub(crate) fn remove_at(&mut self, p: Pos) -> u32 {
        let b = self.seq[p.k];
        let blk = &mut self.blocks[b as usize];
        let r = blk.remove(p.off);
        self.len -= 1;
        if blk.is_empty() {
            blk.shrink_to_fit();
            self.seq.remove(p.k);
            self.spare.push(b);
        }
        r
    }

    /// Keep only the entries `keep` accepts (one pass over everything; for bulk removals).
    pub(crate) fn retain_all(&mut self, mut keep: impl FnMut(u32) -> bool) {
        let mut len = 0;
        for &b in &self.seq {
            let blk = &mut self.blocks[b as usize];
            blk.retain(|&r| keep(r));
            len += blk.len();
        }
        self.len = len;
        let blocks = &mut self.blocks;
        let spare = &mut self.spare;
        self.seq.retain(|&b| {
            let blk = &mut blocks[b as usize];
            if blk.is_empty() {
                blk.shrink_to_fit();
                spare.push(b);
                false
            } else {
                true
            }
        });
        self.restart();
    }

    fn new_block(&mut self, v: Vec<u32>) -> u32 {
        if let Some(id) = self.spare.pop() {
            self.blocks[id as usize] = v;
            id
        } else {
            self.blocks.push(v);
            self.start.push(0);
            (self.blocks.len() - 1) as u32
        }
    }

    /// Rewrite the codes of the entries of blocks `ids` (deduplicated here) and recompute
    /// the block starts.
    pub(crate) fn recode(&mut self, ids: &mut Vec<u32>, rank: &mut [u32]) {
        ids.sort_unstable();
        ids.dedup();
        for &b in ids.iter() {
            for (k, &r) in self.blocks[b as usize].iter().enumerate() {
                rank[r as usize] = (b << OFF_BITS) | k as u32;
            }
        }
        self.restart();
    }

    fn restart(&mut self) {
        self.start.resize(self.blocks.len(), 0);
        let mut pos = 0u32;
        for &b in &self.seq {
            self.start[b as usize] = pos;
            pos += self.blocks[b as usize].len() as u32;
        }
    }
}
