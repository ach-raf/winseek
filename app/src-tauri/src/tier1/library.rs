//! Library window backend: the image timeline, smart collections and the clean-up scans.
//!
//! Everything here reads the Tier 1 store (never Tier 0, except the big-files list, which asks
//! searchd through the normal name source). Nothing deletes anything: the UI sends files to the
//! Recycle Bin through the shell commands and can restore them.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use winseek_store::relevance;
use winseek_store::{cluster_near, ImageRow, Store};

use super::embedder::EmbedError;
use super::{lock, locator_page, stat, Handle, SemanticHit, SemanticResult, Tier1};
use crate::commands::AppState;
use crate::source::Hit;

const COLLECTIONS_KEY: &str = "collections";
const MAX_COLLECTIONS: usize = 50;
/// Most collection members one smart collection shows.
pub const COLLECTION_CAP: usize = 500;
/// Exact-duplicate groups returned (biggest saving first).
const DUP_GROUP_CAP: usize = 1000;
/// Images considered by the near-duplicate scan (newest first).
const NEAR_IMAGE_CAP: usize = 20_000;
/// Near-duplicates are looked for inside 30-day buckets of the modified time.
const BUCKET_MS: i64 = 30 * 24 * 3600 * 1000;
/// Largest number of vectors compared pairwise at once (a bigger bucket is cut into runs).
const BUCKET_CHUNK: usize = 3000;
const BIG_FILES_CAP: usize = 500;

// ---------------------------------------------------------------- DTOs

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LibImage {
    pub path: String,
    pub size: u64,
    /// Modified time, unix ms.
    pub mtime: i64,
}

/// A saved semantic query. `rule` replaces the query with a name rule ("screenshots"), which the UI
/// evaluates against the timeline.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Collection {
    pub name: String,
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub threshold: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DupMember {
    pub path: String,
    pub size: u64,
    pub mtime: i64,
    /// Cosine similarity to the group's newest member (near-duplicates only).
    pub sim: Option<f32>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DupGroupDto {
    pub id: String,
    /// Newest first.
    pub members: Vec<DupMember>,
    /// Bytes freed by keeping only the largest member.
    pub reclaimable: u64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct NearState {
    /// "idle" | "running" | "done" | "error"
    pub state: &'static str,
    pub done: usize,
    pub total: usize,
    pub groups: Vec<DupGroupDto>,
    pub error: Option<String>,
}

#[derive(Default)]
struct LibInner {
    near: Mutex<NearState>,
    /// Bumped by every scan start; an older scan sees the change and stops.
    gen: AtomicU64,
}

/// Tauri managed state: the background near-duplicate scan.
#[derive(Default, Clone)]
pub struct LibState(Arc<LibInner>);

// ---------------------------------------------------------------- collections

/// Threshold of the first, unmeasured defaults. EmbeddingGemma-2 image/query cosines are never that
/// low, so a saved default still carrying it matched every image; loading upgrades it.
const LEGACY_DEFAULT_THRESHOLD: f32 = 0.2;

/// The default collections. Thresholds measured 2026-10-14 with `winseek-eval route` (EmbeddingGemma-2
/// Q8_0, llama.cpp b11588, 418 labelled images): real photos score at most 0.585 / 0.601 / 0.619 /
/// 0.597 against these four queries, members (synthetic receipts + invoices, whiteboards, captioned
/// memes, contracts + forms) score at least 0.691 / 0.614 / 0.634 / 0.645. Since the calibrated gate
/// (2026-11-12) these numbers are no longer raw cosine floors — they are strictness dials (see
/// [`Tier1::threshold_z_min`]); the values were kept so saved collections keep their slider position.
pub fn default_collections() -> Vec<Collection> {
    let c = |name: &str, query: &str, threshold: f32, rule: Option<&str>| Collection {
        name: name.into(),
        query: query.into(),
        threshold,
        rule: rule.map(Into::into),
    };
    vec![
        c("Receipts", "a photo of a paper receipt or invoice", 0.64, None),
        c("Whiteboards", "a whiteboard with handwritten notes and diagrams", 0.61, None),
        c("Memes", "a meme: an image with a funny caption", 0.63, None),
        c("Documents to sign", "a contract or form with a signature line to sign", 0.63, None),
        c("Screenshots", "", 0.0, Some("screenshots")),
    ]
}

/// A saved list from before the thresholds were measured: defaults the user never changed (same
/// name, query and the legacy threshold) get the measured threshold. Anything edited stays.
pub fn upgrade_legacy_thresholds(mut list: Vec<Collection>) -> Vec<Collection> {
    let defaults = default_collections();
    for c in list.iter_mut().filter(|c| c.threshold == LEGACY_DEFAULT_THRESHOLD) {
        if let Some(d) = defaults.iter().find(|d| d.name == c.name && d.query == c.query) {
            c.threshold = d.threshold;
        }
    }
    list
}

/// Trim, validate and cap a user-edited list. Names must be unique (case-insensitive).
pub fn sanitize_collections(list: Vec<Collection>) -> Result<Vec<Collection>, String> {
    if list.len() > MAX_COLLECTIONS {
        return Err(format!("At most {MAX_COLLECTIONS} collections"));
    }
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(list.len());
    for mut c in list {
        c.name = c.name.trim().to_string();
        c.query = c.query.trim().to_string();
        c.rule = c.rule.filter(|r| r == "screenshots");
        if c.name.is_empty() || c.name.chars().count() > 60 {
            return Err("A collection needs a name of 1 to 60 characters".into());
        }
        if c.rule.is_none() && c.query.is_empty() {
            return Err(format!("\"{}\" needs a search phrase", c.name));
        }
        if !seen.insert(c.name.to_lowercase()) {
            return Err(format!("There is already a collection named \"{}\"", c.name));
        }
        c.threshold = if c.threshold.is_finite() { c.threshold.clamp(-1.0, 1.0) } else { 0.0 };
        out.push(c);
    }
    Ok(out)
}

impl Tier1 {
    /// The saved collections, or the defaults while none were ever saved.
    pub fn get_collections(&self) -> Result<Vec<Collection>, String> {
        let raw = lock(&self.store).get_setting(COLLECTIONS_KEY).map_err(|e| e.to_string())?;
        match raw {
            None => Ok(default_collections()),
            Some(s) => Ok(serde_json::from_str(&s).map(upgrade_legacy_thresholds).unwrap_or_else(|_| default_collections())),
        }
    }

    pub fn set_collections(&self, list: Vec<Collection>) -> Result<Vec<Collection>, String> {
        let list = sanitize_collections(list)?;
        let json = serde_json::to_string(&list).map_err(|e| e.to_string())?;
        lock(&self.store).set_setting(COLLECTIONS_KEY, Some(&json)).map_err(|e| e.to_string())?;
        Ok(list)
    }

    /// Every indexed image in scope, newest first.
    pub fn library_images(&self) -> Result<Vec<LibImage>, String> {
        let snap = self.snapshot();
        let rows = lock(&self.store).list_images().map_err(|e| e.to_string())?;
        Ok(rows
            .into_iter()
            .filter(|r| snap.in_scope(&r.path))
            .map(|r| LibImage { path: r.path, size: r.size, mtime: r.mtime })
            .collect())
    }

    /// The stored slider (0.4..0.9, historically a raw cosine floor) as a calibrated-gate
    /// strictness: the hub-corrected z minimum a file must clear (`relevance`). The old midpoint
    /// default 0.62 maps to 4.04 — search's own `Z_MIN` — so saved collections keep their dial
    /// position and gain the calibration; looser (0.4) means "stand out a bit", tighter (0.9)
    /// means "near the top of what the library has for this query".
    pub fn threshold_z_min(threshold: f32) -> f32 {
        (2.5 + (threshold - 0.4) * 7.0).clamp(2.0, 8.0)
    }

    /// Files that belong to a collection: embed the query and return the in-scope files that are
    /// *relevant* — the same calibrated gate search uses (see `relevance`: hub correction, then a
    /// per-modality z-score, so icons that sit near every query cannot flood the list) — best
    /// first, at most `cap`. `threshold` only dials the strictness (see [`Self::threshold_z_min`]).
    pub fn collection_hits(&self, query: &str, threshold: f32, cap: usize) -> Result<SemanticResult, String> {
        let cap = cap.clamp(1, COLLECTION_CAP);
        let q = query.trim();
        if q.is_empty() {
            return Ok(SemanticResult { state: self.state_str().into(), hits: vec![] });
        }
        let (vec, hub) = {
            let mut e = lock(&self.embedder);
            if !e.models_present() {
                return Ok(SemanticResult { state: "models_missing".into(), hits: vec![] });
            }
            match e.embed_query(q).and_then(|v| Ok((v, self.hub_vector(&mut e)?))) {
                Ok(v) => v,
                Err(EmbedError::ModelsMissing) => return Ok(SemanticResult { state: "models_missing".into(), hits: vec![] }),
                Err(EmbedError::Other(m)) => return Err(m),
            }
        };
        let snap = self.snapshot();
        let cands = {
            let store = lock(&self.store);
            // Over-fetch: rows outside the current scopes (scopes shrank since indexing) are dropped.
            store.search_calibrated(&vec, &hub, (cap * 4).max(40)).map_err(|e| e.to_string())?
        };
        let gated = relevance::gate_at(
            cands.into_iter().filter(|c| snap.in_scope(&c.path)).collect(),
            cap,
            Self::threshold_z_min(threshold),
        );
        let hits = gated
            .into_iter()
            .map(|h| {
                let (size, mtime) = stat(&h.path);
                SemanticHit {
                    path: h.path,
                    score: h.score,
                    size,
                    mtime,
                    snippet: None,
                    page: locator_page(h.locator.as_deref()),
                    source: "meaning",
                    boxes: None,
                }
            })
            .collect();
        Ok(SemanticResult { state: self.state_str().into(), hits })
    }

    /// Byte-identical files in scope that still exist, biggest saving first.
    pub fn library_dup_groups(&self) -> Result<Vec<DupGroupDto>, String> {
        let snap = self.snapshot();
        let groups = lock(&self.store).dup_groups().map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for g in groups {
            let members: Vec<DupMember> = g
                .files
                .iter()
                .filter(|f| snap.in_scope(&f.path) && Path::new(&f.path).is_file())
                .map(|f| DupMember { path: f.path.clone(), size: f.size, mtime: f.mtime, sim: None })
                .collect();
            if members.len() < 2 {
                continue;
            }
            let dto = group_dto(g.hash, members);
            out.push(dto);
            if out.len() >= DUP_GROUP_CAP {
                break;
            }
        }
        out.sort_by(|a, b| b.reclaimable.cmp(&a.reclaimable).then_with(|| a.id.cmp(&b.id)));
        Ok(out)
    }
}

fn group_dto(id: String, members: Vec<DupMember>) -> DupGroupDto {
    let total: u64 = members.iter().map(|m| m.size).sum();
    let largest = members.iter().map(|m| m.size).max().unwrap_or(0);
    DupGroupDto { id, members, reclaimable: total.saturating_sub(largest) }
}

// ---------------------------------------------------------------- near-duplicates

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Group visually near-identical images (cosine `>= threshold`), bucket by bucket. `progress(done,
/// total)` is called after every bucket; `cancelled` is polled between buckets. Files with the same
/// content hash count once here (they are the exact-duplicates view's business).
pub fn near_groups(
    store: &Mutex<Store>,
    in_scope: &dyn Fn(&str) -> bool,
    threshold: f32,
    cancelled: &dyn Fn() -> bool,
    progress: &dyn Fn(usize, usize),
) -> Result<Vec<DupGroupDto>, String> {
    let mut imgs: Vec<ImageRow> = lock(store).list_images().map_err(|e| e.to_string())?;
    imgs.retain(|i| in_scope(&i.path));
    imgs.truncate(NEAR_IMAGE_CAP);
    let total = imgs.len();
    progress(0, total);

    // Contiguous runs of the newest-first list that share a bucket key.
    let mut buckets: Vec<(i64, usize)> = Vec::new(); // (bucket key, image count)
    for i in &imgs {
        let k = i.mtime.div_euclid(BUCKET_MS);
        match buckets.last_mut() {
            Some((bk, n)) if *bk == k => *n += 1,
            _ => buckets.push((k, 1)),
        }
    }

    let mut done = 0usize;
    let mut out: Vec<DupGroupDto> = Vec::new();
    for (key, count) in buckets {
        if cancelled() {
            return Err("cancelled".into());
        }
        let rows = lock(store).images_with_vectors(key * BUCKET_MS, (key + 1) * BUCKET_MS).map_err(|e| e.to_string())?;
        let mut seen = HashSet::new();
        let nodes: Vec<(ImageRow, Vec<f32>)> =
            rows.into_iter().filter(|(r, _)| in_scope(&r.path) && seen.insert(r.content_hash.clone())).collect();
        for run in nodes.chunks(BUCKET_CHUNK) {
            if cancelled() {
                return Err("cancelled".into());
            }
            let vecs: Vec<&[f32]> = run.iter().map(|(_, v)| v.as_slice()).collect();
            for g in cluster_near(&vecs, threshold) {
                let first = g[0];
                let members: Vec<DupMember> = g
                    .iter()
                    .filter(|&&i| Path::new(&run[i].0.path).is_file())
                    .map(|&i| DupMember {
                        path: run[i].0.path.clone(),
                        size: run[i].0.size,
                        mtime: run[i].0.mtime,
                        sim: Some(dot(&run[first].1, &run[i].1)),
                    })
                    .collect();
                if members.len() >= 2 {
                    out.push(group_dto(run[first].0.content_hash.clone(), members));
                }
            }
        }
        done += count;
        progress(done.min(total), total);
    }
    // Biggest groups first, then newest.
    out.sort_by(|a, b| {
        b.members.len().cmp(&a.members.len()).then_with(|| b.members[0].mtime.cmp(&a.members[0].mtime))
    });
    Ok(out)
}

impl LibState {
    pub fn snapshot(&self) -> NearState {
        lock(&self.0.near).clone()
    }

    /// Start (or restart) the background scan.
    pub fn start(&self, t: Arc<Tier1>, threshold: f32) {
        let my = self.0.gen.fetch_add(1, Ordering::SeqCst) + 1;
        *lock(&self.0.near) = NearState { state: "running", ..NearState::default() };
        let inner = self.0.clone();
        let spawned = std::thread::Builder::new().name("library-near".into()).spawn(move || {
            super::lower_thread_priority();
            let snap = t.snapshot();
            let in_scope = |p: &str| snap.in_scope(p);
            let cancelled = || inner.gen.load(Ordering::SeqCst) != my;
            let progress = |done: usize, total: usize| {
                if !cancelled() {
                    let mut s = lock(&inner.near);
                    s.done = done;
                    s.total = total;
                }
            };
            let res = near_groups(&t.store, &in_scope, threshold, &cancelled, &progress);
            if cancelled() {
                return; // superseded: the newer scan owns the state
            }
            let mut s = lock(&inner.near);
            match res {
                Ok(groups) => {
                    s.state = "done";
                    s.groups = groups;
                }
                Err(e) => {
                    s.state = "error";
                    s.error = Some(e);
                }
            }
        });
        if let Err(e) = spawned {
            *lock(&self.0.near) = NearState { state: "error", error: Some(e.to_string()), ..NearState::default() };
        }
    }
}

// ---------------------------------------------------------------- big files

/// Tier 0 query for the files under a profile folder, excluding `AppData`.
pub fn big_files_query(profile: &str) -> String {
    format!("file: \"{}\\\" !\"\\AppData\\\"", profile.trim_end_matches('\\'))
}

// ---------------------------------------------------------------- image size

/// Pixel size from the file header (no full decode).
pub fn image_size(path: &str) -> Option<(u32, u32)> {
    image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.into_dimensions().ok()
}

// ---------------------------------------------------------------- Tauri commands

fn tier1(h: &tauri::State<'_, Handle>) -> Result<Arc<Tier1>, String> {
    h.get()
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn library_images(h: tauri::State<'_, Handle>) -> Result<Vec<LibImage>, String> {
    let t = tier1(&h)?;
    blocking(move || t.library_images()).await
}

#[tauri::command]
pub async fn library_collections(h: tauri::State<'_, Handle>) -> Result<Vec<Collection>, String> {
    let t = tier1(&h)?;
    blocking(move || t.get_collections()).await
}

#[tauri::command]
pub async fn library_set_collections(h: tauri::State<'_, Handle>, list: Vec<Collection>) -> Result<Vec<Collection>, String> {
    let t = tier1(&h)?;
    blocking(move || t.set_collections(list)).await
}

#[tauri::command]
pub async fn library_collection(
    h: tauri::State<'_, Handle>,
    query: String,
    threshold: f32,
    cap: Option<usize>,
) -> Result<SemanticResult, String> {
    let t = tier1(&h)?;
    blocking(move || t.collection_hits(&query, threshold, cap.unwrap_or(COLLECTION_CAP))).await
}

#[tauri::command]
pub async fn library_dup_groups(h: tauri::State<'_, Handle>) -> Result<Vec<DupGroupDto>, String> {
    let t = tier1(&h)?;
    blocking(move || t.library_dup_groups()).await
}

#[tauri::command]
pub async fn library_near_start(h: tauri::State<'_, Handle>, ls: tauri::State<'_, LibState>, threshold: f32) -> Result<(), String> {
    let t = tier1(&h)?;
    ls.start(t, threshold.clamp(0.5, 1.0));
    Ok(())
}

#[tauri::command]
pub fn library_near_poll(ls: tauri::State<'_, LibState>) -> NearState {
    ls.snapshot()
}

/// The largest files under the user's profile (Tier 0, sorted by size).
#[tauri::command]
pub async fn library_big_files(state: tauri::State<'_, AppState>, limit: usize) -> Result<Vec<Hit>, String> {
    let profile = std::env::var("USERPROFILE").map_err(|_| "USERPROFILE is not set".to_string())?;
    let src = state.source();
    let q = big_files_query(&profile);
    let limit = limit.clamp(1, BIG_FILES_CAP);
    blocking(move || Ok(src.search(&q, "all", 0, limit, "size").hits)).await
}

#[tauri::command]
pub async fn library_dims(paths: Vec<String>) -> Result<Vec<Option<(u32, u32)>>, String> {
    blocking(move || Ok(paths.iter().take(200).map(|p| image_size(p)).collect())).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tier1::embedder::{index_meta, Embedder, DIMS};
    use winseek_store::{ChunkVec, FileKey, Scopes, VecKind};

    struct Q;
    impl Embedder for Q {
        fn embed_images(&mut self, j: &[Vec<u8>]) -> super::super::embedder::Result<Vec<Vec<f32>>> {
            Ok(j.iter().map(|_| unit(0)).collect())
        }
        fn embed_query(&mut self, q: &str) -> super::super::embedder::Result<Vec<f32>> {
            Ok(unit(if q == "cat" { 1 } else { 2 }))
        }
        fn meta(&self) -> winseek_store::IndexMeta {
            index_meta("test")
        }
    }

    fn unit(i: usize) -> Vec<f32> {
        let mut v = vec![0f32; DIMS];
        v[i] = 1.0;
        v
    }

    /// Unit vector at angle `a` (radians) between axes `i` and `j`.
    fn mix(i: usize, j: usize, a: f32) -> Vec<f32> {
        let mut v = vec![0f32; DIMS];
        v[i] = a.cos();
        v[j] = a.sin();
        v
    }

    fn tier(include: &[&str]) -> Arc<Tier1> {
        let store = Store::open_in_memory(&index_meta("test")).unwrap();
        Tier1::new(store, Box::new(Q), Box::new(crate::tier1::parser::NoParser), Scopes { include: include.iter().map(|s| s.to_string()).collect(), exclude: vec![], ..Default::default() })
    }

    fn add(t: &Tier1, n: u64, path: &str, size: u64, mtime: i64, hash: &str, vec: Vec<f32>) {
        let mut s = lock(&t.store);
        let id = s.upsert_file(&FileKey { vol_guid: "{v}".into(), frn: n }, path, size, mtime, "image").unwrap();
        s.complete(id, hash, &[ChunkVec { chunk: 0, kind: VecKind::Image, locator: None, vec }], &[]).unwrap();
    }

    #[test]
    fn collections_default_roundtrip_and_validation() {
        let t = tier(&[r"C:\Pics"]);
        let d = t.get_collections().unwrap();
        assert_eq!(d.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["Receipts", "Whiteboards", "Memes", "Documents to sign", "Screenshots"]);
        assert_eq!(d[4].rule.as_deref(), Some("screenshots"));

        let saved = t
            .set_collections(vec![Collection { name: "  Cats ".into(), query: " cat ".into(), threshold: 7.0, rule: Some("bogus".into()) }])
            .unwrap();
        assert_eq!(saved, vec![Collection { name: "Cats".into(), query: "cat".into(), threshold: 1.0, rule: None }]);
        assert_eq!(t.get_collections().unwrap(), saved);
        t.set_collections(vec![]).unwrap();
        assert!(t.get_collections().unwrap().is_empty(), "an empty list is a valid choice");

        let bad = |c: Vec<Collection>| t.set_collections(c).unwrap_err();
        let c = |n: &str, q: &str| Collection { name: n.into(), query: q.into(), threshold: 0.2, rule: None };
        assert!(bad(vec![c("", "x")]).contains("name"));
        assert!(bad(vec![c("A", "  ")]).contains("search phrase"));
        assert!(bad(vec![c("A", "x"), c("a", "y")]).contains("already"));
        assert!(t.get_collections().unwrap().is_empty(), "a rejected save changes nothing");
    }

    /// Guard: measured values (see `default_collections`); re-run `winseek-eval route` before changing.
    #[test]
    fn default_thresholds_are_the_measured_ones() {
        let d = default_collections();
        let t: Vec<(&str, f32)> = d.iter().map(|c| (c.name.as_str(), c.threshold)).collect();
        assert_eq!(t, [("Receipts", 0.64), ("Whiteboards", 0.61), ("Memes", 0.63), ("Documents to sign", 0.63), ("Screenshots", 0.0)]);
        // kept for dial continuity: the numbers are strictness dials now (see `threshold_z_min`),
        // and every query collection maps to about search's own gate strictness (z ~ 4)
        assert!(d.iter().filter(|c| c.rule.is_none()).all(|c| (0.6..0.7).contains(&c.threshold)));
    }

    #[test]
    fn saved_legacy_defaults_get_the_measured_thresholds() {
        let t = tier(&[r"C:\Pics"]);
        let mut old = default_collections();
        for c in old.iter_mut().filter(|c| c.rule.is_none()) {
            c.threshold = LEGACY_DEFAULT_THRESHOLD;
        }
        old[1].query = "my own whiteboard query".into(); // edited: keeps 0.2
        old.push(Collection { name: "Cats".into(), query: "cat".into(), threshold: 0.2, rule: None });
        t.set_collections(old).unwrap();
        let got = t.get_collections().unwrap();
        let thr: Vec<f32> = got.iter().map(|c| c.threshold).collect();
        assert_eq!(thr, [0.64, 0.2, 0.63, 0.63, 0.0, 0.2]);
    }

    #[test]
    fn collection_hits_use_the_calibrated_gate_scope_and_cap() {
        let t = tier(&[r"C:\Pics"]);
        // The fake query "cat" embeds to unit(1) and the hub to unit(2), so `adj = raw - dot(unit(2), blob)`:
        add(&t, 1, r"C:\Pics\a.jpg", 1, 1, "h1", mix(1, 2, 0.1)); // raw ~0.995, adj ~0.90 -> passes
        add(&t, 2, r"C:\Pics\f.jpg", 1, 1, "h6", mix(1, 3, 0.2)); // raw ~0.98, orthogonal to the hub, adj ~0.98 -> best
        add(&t, 3, r"C:\Pics\b.jpg", 1, 1, "h2", mix(1, 2, 0.9)); // raw ~0.62 but adj < 0: hub junk, dropped
        add(&t, 4, r"C:\Pics\c.jpg", 1, 1, "h3", mix(1, 2, 1.4)); // far: dropped
        add(&t, 5, r"C:\Pics\e.jpg", 1, 1, "h5", unit(5)); // orthogonal to both: adj 0, dropped
        add(&t, 6, r"C:\Old\d.jpg", 1, 1, "h4", unit(1)); // out of scope
        let r = t.collection_hits("cat", 0.5, 10).unwrap();
        let names: Vec<_> = r.hits.iter().map(|h| h.path.rsplit('\\').next().unwrap()).collect();
        assert_eq!(names, ["f.jpg", "a.jpg"], "calibrated best-first: hub junk and ordinary files stay out");
        assert_eq!(t.collection_hits("cat", 0.5, 1).unwrap().hits.len(), 1, "cap");
        assert_eq!(t.collection_hits("cat", 0.0, 100_000).unwrap().hits.len(), 2, "cap is clamped, gate still applies");
        assert!(t.collection_hits("  ", 0.0, 5).unwrap().hits.is_empty());
    }

    #[test]
    fn threshold_maps_to_gate_strictness() {
        let z = Tier1::threshold_z_min;
        assert!((z(0.62) - 4.04).abs() < 1e-4, "the stored default dials search's own strictness");
        assert_eq!(z(0.4), 2.5);
        assert!((z(0.9) - 6.0).abs() < 1e-4);
        assert!((z(1.0) - 6.7).abs() < 1e-4);
        assert_eq!(z(-1.0), 2.0, "sanitize allows -1; the mapping clamps loose");
    }

    #[test]
    fn library_images_are_scoped_and_newest_first() {
        let t = tier(&[r"C:\Pics"]);
        add(&t, 1, r"C:\Pics\old.jpg", 5, 100, "h1", unit(1));
        add(&t, 2, r"C:\Pics\new.png", 6, 900, "h2", unit(2));
        add(&t, 3, r"C:\Other\x.jpg", 7, 500, "h3", unit(3));
        let v = t.library_images().unwrap();
        assert_eq!(v, vec![
            LibImage { path: r"C:\Pics\new.png".into(), size: 6, mtime: 900 },
            LibImage { path: r"C:\Pics\old.jpg".into(), size: 5, mtime: 100 },
        ]);
    }

    #[test]
    fn dup_groups_skip_missing_files_and_sort_by_saving() {
        let dir = crate::tier1::test_tmp();
        let p = |n: &str| dir.path().join(n).to_string_lossy().into_owned();
        for n in ["a.jpg", "a2.jpg", "b.jpg", "b2.jpg", "b3.jpg", "c.jpg"] {
            std::fs::write(dir.path().join(n), b"x").unwrap();
        }
        let t = tier(&[&dir.path().to_string_lossy()]);
        add(&t, 1, &p("a.jpg"), 10, 1, "ha", unit(1));
        add(&t, 2, &p("a2.jpg"), 10, 2, "ha", unit(1));
        add(&t, 3, &p("b.jpg"), 1000, 1, "hb", unit(2));
        add(&t, 4, &p("b2.jpg"), 1000, 2, "hb", unit(2));
        add(&t, 5, &p("b3.jpg"), 1000, 3, "hb", unit(2));
        add(&t, 6, &p("gone.jpg"), 50, 1, "hc", unit(3)); // not on disk
        add(&t, 7, &p("c.jpg"), 50, 2, "hc", unit(3));
        let g = t.library_dup_groups().unwrap();
        assert_eq!(g.iter().map(|g| (g.id.as_str(), g.members.len(), g.reclaimable)).collect::<Vec<_>>(), vec![("hb", 3, 2000), ("ha", 2, 10)]);
        assert!(g[0].members[0].mtime >= g[0].members[1].mtime, "newest first");
    }

    #[test]
    fn near_groups_cluster_inside_buckets_and_report_progress() {
        let dir = crate::tier1::test_tmp();
        let p = |n: &str| dir.path().join(n).to_string_lossy().into_owned();
        let t = tier(&[&dir.path().to_string_lossy()]);
        let day = 24 * 3600 * 1000;
        let base = 100 * BUCKET_MS + 5 * day;
        let mut n = 0u64;
        let mut put = |name: &str, mtime: i64, hash: &str, v: Vec<f32>| {
            n += 1;
            std::fs::write(dir.path().join(name), b"x").unwrap();
            add(&t, n, &p(name), 100 * n, mtime, hash, v);
        };
        put("burst1.jpg", base, "h1", mix(1, 2, 0.00));
        put("burst2.jpg", base + 1000, "h2", mix(1, 2, 0.05)); // cos ~ 0.9988
        put("burst3.jpg", base + 2000, "h3", mix(1, 2, 0.10)); // cos to 1: ~ 0.995
        put("same_as_1.jpg", base + 3000, "h1", mix(1, 2, 0.00)); // exact dup of burst1: counts once
        put("other.jpg", base + 4000, "h4", unit(5)); // alone
        put("far_away.jpg", base + 60 * day, "h5", mix(1, 2, 0.02)); // similar, but another bucket
        let st = t.snapshot();
        let calls = std::cell::RefCell::new(Vec::new());
        let g = near_groups(&t.store, &|q| st.in_scope(q), 0.95, &|| false, &|d, tot| calls.borrow_mut().push((d, tot))).unwrap();
        assert_eq!(g.len(), 1, "{g:?}");
        let names: Vec<_> = g[0].members.iter().map(|m| m.path.rsplit('\\').next().unwrap().to_string()).collect();
        assert_eq!(names, ["same_as_1.jpg", "burst3.jpg", "burst2.jpg"], "newest first; the exact pair burst1/same_as_1 counts once (its newest file)");
        assert!(g[0].members.iter().all(|m| m.sim.unwrap() > 0.99));
        assert!((g[0].members[0].sim.unwrap() - 1.0).abs() < 1e-5, "similarity to itself");
        let calls = calls.into_inner();
        assert_eq!(calls.first(), Some(&(0, 6)));
        assert_eq!(calls.last(), Some(&(6, 6)));

        // Neighbours in the burst are 0.9988 apart: a stricter cut-off finds nothing.
        let strict = near_groups(&t.store, &|q| st.in_scope(q), 0.9995, &|| false, &|_, _| {}).unwrap();
        assert!(strict.is_empty());
    }

    #[test]
    fn near_groups_can_be_cancelled() {
        let t = tier(&[r"C:\Pics"]);
        add(&t, 1, r"C:\Pics\a.jpg", 1, 1, "h1", unit(1));
        let st = t.snapshot();
        let r = near_groups(&t.store, &|q| st.in_scope(q), 0.95, &|| true, &|_, _| {});
        assert_eq!(r.unwrap_err(), "cancelled");
    }

    #[test]
    fn big_files_query_targets_the_profile_without_appdata() {
        assert_eq!(big_files_query(r"C:\Users\User"), "file: \"C:\\Users\\User\\\" !\"\\AppData\\\"");
        assert_eq!(big_files_query("C:\\Users\\User\\"), big_files_query(r"C:\Users\User"));
    }

    #[test]
    fn image_size_reads_the_header() {
        let dir = crate::tier1::test_tmp();
        let f = dir.path().join("a.png");
        image::RgbImage::from_pixel(30, 12, image::Rgb([1, 2, 3])).save(&f).unwrap();
        assert_eq!(image_size(&f.to_string_lossy()), Some((30, 12)));
        assert_eq!(image_size(&dir.path().join("nope.png").to_string_lossy()), None);
    }
}
