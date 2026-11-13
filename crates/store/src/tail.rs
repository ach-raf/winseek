//! Progressive indexing of long documents.
//!
//! A long PDF is indexed in two steps: its first pages are extracted, embedded and stored like any
//! other file (so they are searchable at once), and the rest is queued here as a *tail* that is
//! worked off in slices when nothing else is waiting. The tail only ever appends chunks to the
//! content that the head created; it never changes what is already searchable.

use rusqlite::params;

use crate::{ChunkVec, Error, Result, Store, STATE_DONE, STATE_FAILED};

/// One queued tail: pages `next_page..=total` of the content `hash` are still to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailJob {
    pub hash: String,
    /// 1-based first page not indexed yet.
    pub next_page: u32,
    /// Last page that will be indexed (the page cap is applied by the caller).
    pub total: u32,
    pub attempts: u32,
    /// A live file with this content.
    pub path: String,
}

/// Tail progress for the status page.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TailCounts {
    /// Documents with pages still to do.
    pub queued: u64,
    /// Pages still to do over all of them.
    pub pages_left: u64,
}

impl Store {
    /// Queue the pages `next_page..=total` of `hash` (no-op if the content already has a tail row).
    pub fn queue_tail(&mut self, hash: &str, next_page: u32, total: u32) -> Result<bool> {
        if next_page > total {
            return Ok(false);
        }
        let n = self
            .conn
            .execute("INSERT OR IGNORE INTO pdf_tail(hash, next_page, total) VALUES (?1, ?2, ?3)", params![hash, next_page, total])?;
        Ok(n > 0)
    }

    /// Up to `limit` tails, newest file first. Only content whose head is complete (every file with
    /// it `done`, nothing waiting for vectors) is offered, and only while a live file has it.
    pub fn next_tail(&self, limit: usize) -> Result<Vec<TailJob>> {
        let mut st = self.conn.prepare_cached(
            "SELECT t.hash, t.next_page, t.total, t.attempts, f.path, MAX(f.mtime)
             FROM pdf_tail t JOIN files f ON f.content_hash = t.hash AND f.state = 1
             WHERE t.state = 0 AND t.hash NOT IN (SELECT hash FROM pending_vecs)
             GROUP BY t.hash ORDER BY MAX(f.mtime) DESC, t.hash LIMIT ?1",
        )?;
        let rows = st.query_map([limit as i64], |r| {
            Ok(TailJob { hash: r.get(0)?, next_page: r.get(1)?, total: r.get(2)?, attempts: r.get(3)?, path: r.get(4)? })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// The first chunk number of `hash` that is free (chunks are unique across texts and images).
    pub fn next_chunk(&self, hash: &str) -> Result<u32> {
        let n: Option<i64> = self.conn.query_row(
            "SELECT MAX(c) FROM (SELECT MAX(chunk) AS c FROM vectors WHERE hash = ?1
                                 UNION ALL SELECT MAX(CAST(chunk AS INTEGER)) FROM texts WHERE hash = ?1
                                 UNION ALL SELECT MAX(chunk) FROM pending_vecs WHERE hash = ?1)",
            [hash],
            |r| r.get(0),
        )?;
        Ok(n.map_or(0, |n| n as u32 + 1))
    }

    /// Add the chunks of one slice and move the cursor to `next_page`; the tail is done when that is
    /// past `total`. One transaction: a crash leaves the cursor and the chunks in step.
    pub fn append_tail(&mut self, hash: &str, next_page: u32, vectors: &[ChunkVec], texts: &[(u32, String)]) -> Result<()> {
        for v in vectors {
            if v.vec.len() != self.dims {
                return Err(Error::BadVector { expected: self.dims, got: v.vec.len() });
            }
        }
        let tx = self.conn.transaction()?;
        {
            let mut ins =
                tx.prepare_cached("INSERT OR REPLACE INTO vectors(hash, chunk, kind, locator, vec) VALUES (?1,?2,?3,?4,?5)")?;
            for v in vectors {
                let blob: Vec<u8> = v.vec.iter().flat_map(|x| x.to_le_bytes()).collect();
                ins.execute(params![hash, v.chunk, v.kind.as_str(), v.locator, blob])?;
            }
            let mut ins_t = tx.prepare_cached("INSERT INTO texts(hash, chunk, body) VALUES (?1,?2,?3)")?;
            for (chunk, body) in texts {
                ins_t.execute(params![hash, chunk, body])?;
            }
        }
        tx.execute(
            "UPDATE pdf_tail SET next_page = ?2, attempts = 0, error = NULL,
                    state = CASE WHEN ?2 > total THEN ?3 ELSE state END WHERE hash = ?1",
            params![hash, next_page, STATE_DONE],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Count a failed slice; returns the attempts so far.
    pub fn bump_tail(&mut self, hash: &str, error: &str) -> Result<u32> {
        self.conn
            .execute("UPDATE pdf_tail SET attempts = attempts + 1, error = ?2 WHERE hash = ?1", params![hash, error])?;
        Ok(self.conn.query_row("SELECT attempts FROM pdf_tail WHERE hash = ?1", [hash], |r| r.get(0)).unwrap_or(0))
    }

    /// Give up on the rest of this document; what the head and earlier slices stored stays.
    pub fn fail_tail(&mut self, hash: &str, error: &str) -> Result<()> {
        self.conn.execute("UPDATE pdf_tail SET state = ?2, error = ?3 WHERE hash = ?1", params![hash, STATE_FAILED, error])?;
        Ok(())
    }

    pub fn tail_counts(&self) -> Result<TailCounts> {
        let (queued, pages_left): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(total - next_page + 1), 0) FROM pdf_tail WHERE state = 0",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(TailCounts { queued: queued as u64, pages_left: pages_left as u64 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FileKey, IndexMeta, VecKind};

    fn store() -> Store {
        let meta = IndexMeta { model_id: "t".into(), llama_build: "b".into(), quant: "q".into(), image_budget: 280, dims: 4 };
        Store::open_in_memory(&meta).unwrap()
    }

    fn cv(chunk: u32, v: [f32; 4]) -> ChunkVec {
        ChunkVec { chunk, kind: VecKind::Text, locator: Some(format!("{{\"page\":{}}}", chunk + 1)), vec: v.to_vec() }
    }

    #[test]
    fn head_is_searchable_then_the_tail_appends_without_touching_it() {
        let mut s = store();
        let id = s.upsert_file(&FileKey { vol_guid: "v".into(), frn: 1 }, r"C:\p\long.pdf", 10, 1, "pdf").unwrap();
        s.complete(id, "h", &[cv(0, [1., 0., 0., 0.]), cv(1, [0., 1., 0., 0.])], &[(0, "alpha head".into()), (1, "bravo head".into())]).unwrap();
        assert!(s.queue_tail("h", 3, 5).unwrap());
        assert!(!s.queue_tail("h", 3, 5).unwrap(), "queued once");
        assert!(!s.queue_tail("h2", 6, 5).unwrap(), "nothing to do past the last page");
        assert_eq!(s.search_text("alpha", 5).unwrap().len(), 1, "head searchable before any tail work");
        assert_eq!(s.search_text("omega", 5).unwrap().len(), 0);
        assert_eq!(s.tail_counts().unwrap(), TailCounts { queued: 1, pages_left: 3 });

        let job = s.next_tail(4).unwrap().remove(0);
        assert_eq!((job.next_page, job.total, job.path.as_str()), (3, 5, r"C:\p\long.pdf"));
        assert_eq!(s.next_chunk("h").unwrap(), 2);
        s.append_tail("h", 5, &[cv(2, [0., 0., 1., 0.])], &[(2, "charlie omega".into())]).unwrap();
        assert_eq!(s.search_text("omega", 5).unwrap().len(), 1, "tail text is searchable once its slice lands");
        assert_eq!(s.search_text("alpha", 5).unwrap().len(), 1, "head untouched");
        assert_eq!(s.next_chunk("h").unwrap(), 3);
        assert_eq!(s.search_vectors(&[0., 0., 1., 0.], 3, None).unwrap()[0].locator.as_deref(), Some("{\"page\":3}"));
        assert_eq!(s.next_tail(4).unwrap()[0].next_page, 5, "cursor moved");

        s.append_tail("h", 6, &[], &[(3, "delta".into())]).unwrap();
        assert!(s.next_tail(4).unwrap().is_empty(), "past the last page: done");
        assert_eq!(s.tail_counts().unwrap(), TailCounts::default());
    }

    #[test]
    fn failures_count_and_give_up_keeps_what_was_stored() {
        let mut s = store();
        let id = s.upsert_file(&FileKey { vol_guid: "v".into(), frn: 1 }, r"C:\p\a.pdf", 10, 1, "pdf").unwrap();
        s.complete(id, "h", &[], &[(0, "kept text".into())]).unwrap();
        s.queue_tail("h", 2, 9).unwrap();
        assert_eq!(s.bump_tail("h", "boom").unwrap(), 1);
        assert_eq!(s.bump_tail("h", "boom").unwrap(), 2);
        s.fail_tail("h", "boom").unwrap();
        assert!(s.next_tail(4).unwrap().is_empty());
        assert_eq!(s.search_text("kept", 5).unwrap().len(), 1);
    }

    #[test]
    fn a_tail_waits_while_the_head_still_owes_vectors_and_dies_with_its_file() {
        let mut s = store();
        let id = s.upsert_file(&FileKey { vol_guid: "v".into(), frn: 1 }, r"C:\p\a.pdf", 10, 1, "pdf").unwrap();
        s.complete_text_only(id, "h", &[crate::PendingVec { chunk: 0, kind: VecKind::Text, locator: None }], &[(0, "words".into())]).unwrap();
        s.queue_tail("h", 2, 9).unwrap();
        assert!(s.next_tail(4).unwrap().is_empty(), "head not done (text-only)");
        s.finish_vectors("h", &[cv(0, [1., 0., 0., 0.])]).unwrap();
        assert_eq!(s.next_tail(4).unwrap().len(), 1);
        s.remove(&FileKey { vol_guid: "v".into(), frn: 1 }).unwrap();
        s.gc().unwrap();
        assert_eq!(s.tail_counts().unwrap(), TailCounts::default(), "gc drops the tail of gone content");
    }
}
