//! The live path: corpus -> (same image preprocessing as the app) -> embeddings -> in-memory
//! `Store` -> vector search per case. The embedder is a trait so the pipeline is unit-testable.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::json;
use winseek_llama::embed::truncate_normalize;
use winseek_llama::install::find_server_exe;
use winseek_llama::manifest::ModelRole;
use winseek_llama::prep::prepare_jpeg;
use winseek_llama::server::{build_args, Role, SpawnConfig};
use winseek_llama::{sha256_file, sha256_hex, EmbedClient, LlamaServer, Manifest, ModelProfile};
use winseek_store::code_rank;
use winseek_store::{ChunkVec, CodeWordsHit, FileKey, IndexMeta, Store, VecKind};

use crate::corpus::{load_index, CorpusIndex};
use crate::metrics::{rank_of, Case, CaseResult, Report};

/// Must match the app (`app/src-tauri/src/tier1/embedder.rs`).
pub const MODEL_ID: &str = "embeddinggemma-2";
pub const QUANT: &str = "Q8_0";
pub const IMAGE_BUDGET: u32 = 280;
pub const DIMS: usize = 768;
/// Images per request, as small as the app's pass batch.
const IMAGE_BATCH: usize = 8;

/// Raw (un-truncated) embeddings, exactly what `EmbedClient` returns.
pub trait Embed {
    fn images(&mut self, jpegs: &[&[u8]]) -> Result<Vec<Vec<f32>>, String>;
    fn documents(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>, String>;
    /// `(title, text)` pairs — an empty title is the untitled form.
    fn documents_titled(&mut self, docs: &[(&str, &str)]) -> Result<Vec<Vec<f32>>, String>;
    fn queries(&mut self, queries: &[&str]) -> Result<Vec<Vec<f32>>, String>;
    /// Code-retrieval task form (`task: code retrieval | query: ...`). The default falls back
    /// to the generic query form so test fakes compile unchanged.
    fn code_queries(&mut self, queries: &[&str]) -> Result<Vec<Vec<f32>>, String> {
        self.queries(queries)
    }
}

impl Embed for EmbedClient {
    fn images(&mut self, jpegs: &[&[u8]]) -> Result<Vec<Vec<f32>>, String> {
        self.embed_images_jpeg(jpegs).map_err(|e| e.to_string())
    }
    fn documents(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>, String> {
        self.embed_documents(texts).map_err(|e| e.to_string())
    }
    fn documents_titled(&mut self, docs: &[(&str, &str)]) -> Result<Vec<Vec<f32>>, String> {
        self.embed_documents_titled(docs).map_err(|e| e.to_string())
    }
    fn queries(&mut self, queries: &[&str]) -> Result<Vec<Vec<f32>>, String> {
        self.embed_queries(queries).map_err(|e| e.to_string())
    }
    fn code_queries(&mut self, queries: &[&str]) -> Result<Vec<Vec<f32>>, String> {
        self.embed_queries_code(queries).map_err(|e| e.to_string())
    }
}

pub fn finish(raw: Vec<Vec<f32>>) -> Result<Vec<Vec<f32>>, String> {
    raw.iter()
        .map(|v| truncate_normalize(v, DIMS).map_err(|e| e.to_string()))
        .collect()
}

pub fn index_meta(llama_build: &str) -> IndexMeta {
    IndexMeta {
        model_id: MODEL_ID.into(),
        llama_build: llama_build.into(),
        quant: QUANT.into(),
        image_budget: IMAGE_BUDGET,
        dims: DIMS as u32,
    }
}

/// Embed every corpus file into a fresh in-memory store. With `titles`, every text file is
/// embedded as `title: {file name} | text: {body}` instead of the untitled form.
/// Returns the store.
pub fn build_store(
    dir: &Path,
    corpus: &CorpusIndex,
    emb: &mut dyn Embed,
    llama_build: &str,
    titles: bool,
) -> Result<Store, String> {
    let mut store = Store::open_in_memory(&index_meta(llama_build)).map_err(|e| e.to_string())?;
    let mut images: Vec<(usize, Vec<u8>, String)> = Vec::new(); // (file no, jpeg, hash)
    let mut texts: Vec<(usize, String, String)> = Vec::new();
    let mut ids = Vec::new();
    for (i, f) in corpus.files.iter().enumerate() {
        let path = dir.join(&f.name);
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let hash = sha256_hex(&bytes);
        let id = store
            .upsert_file(
                &FileKey {
                    vol_guid: "{eval}".into(),
                    frn: i as u64,
                },
                &path.to_string_lossy(),
                bytes.len() as u64,
                0,
                &f.kind,
            )
            .map_err(|e| e.to_string())?;
        ids.push((id, hash.clone()));
        match f.kind.as_str() {
            "image" => images.push((
                i,
                prepare_jpeg(&bytes).map_err(|e| format!("{}: {e}", f.name))?,
                hash,
            )),
            "text" => texts.push((i, String::from_utf8_lossy(&bytes).into_owned(), hash)),
            other => return Err(format!("{}: unknown kind {other}", f.name)),
        }
    }
    for batch in images.chunks(IMAGE_BATCH) {
        let refs: Vec<&[u8]> = batch.iter().map(|(_, j, _)| j.as_slice()).collect();
        let vecs = finish(emb.images(&refs)?)?;
        if vecs.len() != batch.len() {
            return Err(format!(
                "expected {} image vectors, got {}",
                batch.len(),
                vecs.len()
            ));
        }
        for ((i, _, hash), vec) in batch.iter().zip(vecs) {
            let chunk = ChunkVec {
                chunk: 0,
                kind: VecKind::Image,
                locator: None,
                vec,
            };
            store
                .complete(ids[*i].0, hash, &[chunk], &[])
                .map_err(|e| e.to_string())?;
        }
    }
    for batch in texts.chunks(IMAGE_BATCH) {
        let vecs = if titles {
            let refs: Vec<(&str, &str)> = batch
                .iter()
                .map(|(i, t, _)| (corpus.files[*i].name.as_str(), t.as_str()))
                .collect();
            finish(emb.documents_titled(&refs)?)?
        } else {
            let refs: Vec<&str> = batch.iter().map(|(_, t, _)| t.as_str()).collect();
            finish(emb.documents(&refs)?)?
        };
        if vecs.len() != batch.len() {
            return Err(format!(
                "expected {} text vectors, got {}",
                batch.len(),
                vecs.len()
            ));
        }
        for ((i, body, hash), vec) in batch.iter().zip(vecs) {
            let chunk = ChunkVec {
                chunk: 0,
                kind: VecKind::Text,
                locator: None,
                vec,
            };
            store
                .complete(ids[*i].0, hash, &[chunk], &[(0, body.clone())])
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(store)
}

fn file_name_of(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Run every case against the store (query -> embed -> `search_vectors` over all files).
///
/// `hybrid` ranks like the app does: the vector list fused with the exact-word (FTS) hits.
pub fn run_cases(
    store: &Store,
    cases: &[Case],
    emb: &mut dyn Embed,
    n_files: usize,
    hybrid: bool,
) -> Result<Vec<CaseResult>, String> {
    let mut out = Vec::with_capacity(cases.len());
    for c in cases {
        let q = finish(emb.queries(&[c.q.as_str()])?)?
            .into_iter()
            .next()
            .ok_or("empty query embedding")?;
        let hits = store
            .search_vectors(&q, n_files.max(5), None)
            .map_err(|e| e.to_string())?;
        let mut ranked: Vec<String> = hits.iter().map(|h| file_name_of(&h.path)).collect();
        if hybrid {
            let text: Vec<String> = store
                .search_text(&winseek_store::fts_escape(&c.q), n_files.max(5))
                .map_err(|e| e.to_string())?
                .iter()
                .filter(|t| t.snippet.contains('['))
                .map(|t| file_name_of(&t.path))
                .collect();
            ranked = winseek_store::fuse::fuse_order(
                &ranked,
                &text,
                winseek_store::fuse::exact_lean(&c.q),
            );
        }
        out.push(CaseResult {
            q: c.q.clone(),
            tag: c.tag.clone(),
            expect: c.expect.clone(),
            rank: rank_of(&ranked, &c.expect),
            top: ranked.into_iter().take(5).collect(),
        });
    }
    Ok(out)
}

/// The indexed code corpus: the store plus every row's corpus-relative path (what the cases'
/// expectations glob). Keys are the store's path strings.
pub struct CodeIndex {
    pub store: Store,
    rel_of: HashMap<String, String>,
}

/// Index the code corpus exactly like the app's text route does: the store crate's stretch
/// chunker (whole lines, 1-based `loc`/`loc_end` locators), every stretch embedded under its
/// repo-relative path as title, FTS rows per stretch.
pub fn build_code_store(
    dir: &Path,
    corpus: &CorpusIndex,
    emb: &mut dyn Embed,
    llama_build: &str,
) -> Result<CodeIndex, String> {
    let repo = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "code".into());
    let mut store = Store::open_in_memory(&index_meta(llama_build)).map_err(|e| e.to_string())?;
    // (file no, chunk no, title, trimmed body, locator, hash)
    let mut stretches: Vec<(usize, u32, String, String, String, String)> = Vec::new();
    let mut rel_of = HashMap::new();
    let mut ids = Vec::new();
    for (i, f) in corpus.files.iter().enumerate() {
        let path = dir.join(&f.name);
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let hash = sha256_hex(&bytes);
        let store_path = path.to_string_lossy().into_owned();
        let id = store
            .upsert_file(
                &FileKey {
                    vol_guid: "{eval}".into(),
                    frn: i as u64,
                },
                &store_path,
                bytes.len() as u64,
                0,
                &f.kind,
            )
            .map_err(|e| e.to_string())?;
        ids.push((id, hash.clone()));
        rel_of.insert(store_path, f.name.replace('\\', "/"));
        let text = String::from_utf8_lossy(&bytes)
            .replace("\r\n", "\n")
            .replace('\r', "\n");
        let title = format!("{}\\{}", repo, f.name.replace('/', "\\"));
        for (chunk_no, st) in winseek_store::code_chunk::code_stretches(
            &text,
            winseek_store::code_chunk::CODE_STRETCH,
        )
        .into_iter()
        .enumerate()
        {
            stretches.push((
                i,
                chunk_no as u32,
                title.clone(),
                st.text.trim().to_owned(),
                winseek_store::code_chunk::code_locator(st.first, st.last),
                hash.clone(),
            ));
        }
    }
    for batch in stretches.chunks(IMAGE_BATCH) {
        let refs: Vec<(&str, &str)> = batch
            .iter()
            .map(|(_, _, t, b, _, _)| (t.as_str(), b.as_str()))
            .collect();
        let vecs = finish(emb.documents_titled(&refs)?)?;
        if vecs.len() != batch.len() {
            return Err(format!(
                "expected {} stretch vectors, got {}",
                batch.len(),
                vecs.len()
            ));
        }
        for ((file_no, chunk_no, _, body, locator, hash), vec) in batch.iter().zip(vecs) {
            let chunk = ChunkVec {
                chunk: *chunk_no,
                kind: VecKind::Text,
                locator: Some(locator.clone()),
                vec,
            };
            store
                .complete(
                    ids[*file_no].0,
                    hash,
                    &[chunk],
                    &[(*chunk_no, body.clone())],
                )
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(CodeIndex { store, rel_of })
}

/// Run the code cases the way the app's `code:` search does: the query embeds in the code
/// retrieval task form, candidates come from the code partition's relevance scale, words come
/// from bm25 over the stretches, one score (`code_rank`). Ranks by (relative path, locator) so
/// `path@first-last` expectations check the file AND the stretch. The list is pre-focus: the
/// app's definition/first-line pass only repoints where a shown hit opens, it never reorders.
pub fn run_code_cases(
    idx: &CodeIndex,
    cases: &[Case],
    emb: &mut dyn Embed,
    n_files: usize,
) -> Result<Vec<CaseResult>, String> {
    // The hub vector, exactly what the app's search carries (generic-query form prompts).
    let bank = finish(emb.queries(&code_rank_bank_prompts())?)?;
    let hub = winseek_store::relevance::hub_vector(&bank);
    let store = &idx.store;
    let mut out = Vec::with_capacity(cases.len());
    for c in cases {
        let qv = finish(emb.code_queries(&[c.q.as_str()])?)?
            .into_iter()
            .next()
            .ok_or("empty query embedding")?;
        let id = code_rank::identifier(&c.q);
        let terms = code_rank::terms(&c.q);
        let patterns = match &id {
            Some(w) => vec![code_rank::phrase(w)],
            None => {
                code_rank::word_patterns(&terms, &|p| store.code_has_pattern(p).unwrap_or(false))
            }
        };
        let lookup = id.is_some() || code_rank::is_lookup(&terms);
        // (score, rel path, locator)
        let mut scored: Vec<(f32, String, Option<String>)> = Vec::new();
        let mut seen: HashMap<String, ()> = HashMap::new();
        if !patterns.is_empty() {
            let words: Vec<CodeWordsHit> = store
                .search_code_words(&patterns.join(" OR "), n_files.max(5) * 8)
                .unwrap_or_default();
            let mut best_words: HashMap<&str, &CodeWordsHit> = HashMap::new();
            for h in &words {
                best_words.entry(h.hash.as_str()).or_insert(h);
            }
            let mut coverage = std::collections::HashMap::new();
            if !words.is_empty() {
                let mut hashes: Vec<String> = words.iter().map(|h| h.hash.clone()).collect();
                hashes.sort();
                hashes.dedup();
                let pats: Vec<&str> = patterns.iter().map(String::as_str).collect();
                coverage = store.code_coverage(&pats, &hashes).unwrap_or_default();
            }
            let cands = store.search_code_candidates(&qv, &hub).unwrap_or_default();
            for cd in &cands {
                let key = cd.path.to_lowercase();
                if seen.insert(key, ()).is_some() {
                    continue;
                }
                let cov = coverage.get(&cd.hash).copied().unwrap_or(0.0);
                if !cd.passes() && cov < code_rank::COVERAGE_CANDIDATE {
                    continue;
                }
                let by_meaning = cd.passes();
                let locator = if by_meaning {
                    cd.locator.clone()
                } else {
                    best_words
                        .get(cd.hash.as_str())
                        .and_then(|w| w.locator.clone())
                };
                let score = code_rank::score(cd.z, cov, lookup);
                if let Some(rel) = idx.rel_of.get(&cd.path) {
                    scored.push((score, rel.clone(), locator));
                }
            }
            // Files answered by words alone (no vector: models missing or owed).
            for h in &words {
                if seen.contains_key(&h.path.to_lowercase()) {
                    continue;
                }
                let cov = coverage.get(&h.hash).copied().unwrap_or(0.0);
                if cov < code_rank::COVERAGE_CANDIDATE {
                    continue;
                }
                seen.insert(h.path.to_lowercase(), ());
                let score = code_rank::score(None, cov, lookup);
                if let Some(rel) = idx.rel_of.get(&h.path) {
                    scored.push((score, rel.clone(), h.locator.clone()));
                }
            }
        }
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let ranked: Vec<(String, Option<String>)> =
            scored.into_iter().map(|(_, r, l)| (r, l)).collect();
        let rank = crate::metrics::rank_code(&ranked, &c.expect);
        out.push(CaseResult {
            q: c.q.clone(),
            tag: c.tag.clone(),
            expect: c.expect.clone(),
            rank,
            top: ranked.iter().take(5).map(|(r, _)| r.clone()).collect(),
        });
    }
    Ok(out)
}

/// The hub prompts as owned strings (the trait takes `&[&str]`).
fn code_rank_bank_prompts() -> Vec<&'static str> {
    winseek_store::relevance::HUB_PROMPTS.to_vec()
}

/// Embedding-model file names from the pinned manifest (same entry the app loads).
pub fn model_files(models_dir: &Path) -> Result<(PathBuf, PathBuf), String> {
    let man = Manifest::embedded();
    let entry = man
        .model(MODEL_ID, ModelProfile::Default)
        .ok_or("manifest has no embeddinggemma-2 entry")?;
    let pick = |role: ModelRole| {
        entry
            .files
            .iter()
            .find(|f| f.role == role)
            .map(|f| models_dir.join(f.file_name()))
            .ok_or_else(|| format!("manifest entry lacks {role:?} file"))
    };
    Ok((pick(ModelRole::Model)?, pick(ModelRole::Mmproj)?))
}

pub struct RunOptions {
    pub corpus: PathBuf,
    pub cases: PathBuf,
    pub models: PathBuf,
    pub llama: PathBuf,
    pub url: Option<String>,
    pub api_key: String,
    /// Rank like the app: vector list fused with the exact-word hits.
    pub hybrid: bool,
    /// Embed text documents with `title: {file name} | text: {body}`.
    pub titles: bool,
}

fn version_of(exe: &Path) -> String {
    Command::new(exe)
        .arg("--version")
        .current_dir(exe.parent().unwrap_or(Path::new(".")))
        .output()
        .map(|o| {
            String::from_utf8_lossy(&[o.stdout, o.stderr].concat())
                .lines()
                .find(|l| l.contains("version"))
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .unwrap_or_default()
}

/// A vision-enabled embedding client. Holds the spawned llama-server (if any) alive; Drop kills it.
pub struct Connection {
    pub client: EmbedClient,
    _server: Option<LlamaServer>,
}

/// Connect to `opts.url`, or spawn llama-server with the app's EmbedIndex preset. Records how in
/// `meta["server"]` (exe, version, exact preset args, model hashes).
pub fn connect(opts: &RunOptions, meta: &mut serde_json::Value) -> Result<Connection, String> {
    let (model, mmproj) = model_files(&opts.models)?;
    let mut _server: Option<LlamaServer> = None;
    let (base_url, key) = match &opts.url {
        Some(u) => {
            meta["server"] = json!({ "url": u, "spawned": false });
            (u.clone(), opts.api_key.clone())
        }
        None => {
            for p in [&model, &mmproj] {
                if !p.is_file() {
                    return Err(format!("model file missing: {}", p.display()));
                }
            }
            let exe = find_server_exe(&opts.llama)
                .ok_or_else(|| format!("no llama-server under {}", opts.llama.display()))?;
            eprintln!("hashing model files...");
            let log = std::env::temp_dir().join("winseek-eval-llama.log");
            let cfg = SpawnConfig {
                role: Role::EmbedIndex,
                log_file: Some(log.clone()),
                ..Default::default()
            };
            eprintln!("spawning {} (log: {})", exe.display(), log.display());
            let server = LlamaServer::spawn_with(&exe, &model, Some(&mmproj), &[], &cfg)
                .map_err(|e| e.to_string())?;
            // The exact argument list the preset produced, with the throwaway key redacted.
            let args = build_args(
                &model,
                Some(&mmproj),
                server.port(),
                "***",
                Role::EmbedIndex,
                Default::default(),
                &[],
            );
            meta["server"] = json!({
                "spawned": true,
                "exe": exe.display().to_string(),
                "version": version_of(&exe),
                "llama_dir": opts.llama.display().to_string(),
                "preset_args": args,
                "model_sha256": sha256_file(&model).map_err(|e| e.to_string())?,
                "mmproj_sha256": sha256_file(&mmproj).map_err(|e| e.to_string())?,
            });
            let out = (server.base_url(), server.api_key().to_string());
            _server = Some(server);
            out
        }
    };
    meta["unix_time"] = json!(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0));
    meta["titles"] = json!(opts.titles);

    let client = EmbedClient::new(&base_url, &key);
    match client.vision_enabled() {
        Ok(true) => Ok(Connection { client, _server }),
        Ok(false) => Err("llama-server has no vision (mmproj not loaded)".into()),
        Err(e) => Err(format!("/props: {e}")),
    }
}

/// Full live run. Spawns llama-server (unless `url` is given) and drops it at the end.
pub fn run(opts: &RunOptions) -> Result<Report, String> {
    let corpus = load_index(&opts.corpus)?;
    let cases_text = std::fs::read_to_string(&opts.cases)
        .map_err(|e| format!("{}: {e}", opts.cases.display()))?;
    let cases = crate::metrics::parse_cases(&cases_text)?;
    let manifest = Manifest::embedded();
    let mut meta = json!({
        "manifest_build": manifest.llama.build,
        "dims": DIMS,
        "quant": QUANT,
        "image_budget": IMAGE_BUDGET,
        "corpus_files": corpus.files.len(),
        "cases": cases.len(),
        "hybrid": opts.hybrid,
    });
    let mut conn = connect(opts, &mut meta)?;
    let client = &mut conn.client;
    let started = std::time::Instant::now();
    eprintln!("embedding {} corpus files...", corpus.files.len());
    let is_code = corpus.files.iter().any(|f| f.kind == "code");
    let results = if is_code {
        eprintln!("code corpus: indexing stretches with the app's chunker...");
        let idx = build_code_store(&opts.corpus, &corpus, client, &manifest.llama.build)?;
        meta["code"] = json!(true);
        meta["index_secs"] = json!((started.elapsed().as_secs_f64() * 100.0).round() / 100.0);
        eprintln!("running {} code cases...", cases.len());
        run_code_cases(&idx, &cases, client, corpus.files.len())?
    } else {
        let store = build_store(
            &opts.corpus,
            &corpus,
            client,
            &manifest.llama.build,
            opts.titles,
        )?;
        meta["index_secs"] = json!((started.elapsed().as_secs_f64() * 100.0).round() / 100.0);
        eprintln!("running {} cases...", cases.len());
        run_cases(&store, &cases, client, corpus.files.len(), opts.hybrid)?
    };
    Ok(Report::new(meta, results))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus;

    /// Fake backend: every image/doc gets a one-hot vector at its arrival order; a query
    /// `"k:N"` is one-hot at N. Lets us check wiring (store, ranking, names) without a model.
    struct Fake {
        next: usize,
    }

    fn onehot(i: usize) -> Vec<f32> {
        let mut v = vec![0.0; DIMS];
        v[i] = 1.0;
        v
    }

    impl Embed for Fake {
        fn images(&mut self, jpegs: &[&[u8]]) -> Result<Vec<Vec<f32>>, String> {
            // prepare_jpeg output must really be a JPEG, as the server expects
            assert!(jpegs.iter().all(|j| j.starts_with(&[0xff, 0xd8, 0xff])));
            Ok(jpegs
                .iter()
                .map(|_| {
                    self.next += 1;
                    onehot(self.next - 1)
                })
                .collect())
        }
        fn documents(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>, String> {
            Ok(texts
                .iter()
                .map(|_| {
                    self.next += 1;
                    onehot(self.next - 1)
                })
                .collect())
        }
        fn documents_titled(&mut self, docs: &[(&str, &str)]) -> Result<Vec<Vec<f32>>, String> {
            self.documents(docs.iter().map(|(_, t)| *t).collect::<Vec<_>>().as_slice())
        }
        fn queries(&mut self, queries: &[&str]) -> Result<Vec<Vec<f32>>, String> {
            Ok(queries
                .iter()
                .map(|q| match q.strip_prefix("k:") {
                    Some(n) => onehot(n.parse().unwrap()),
                    // Bare words (the hub prompts): a deterministic pseudo-vector so the
                    // words-side tests stay independent of arrival order.
                    None => {
                        let h = q
                            .bytes()
                            .fold(7usize, |a, b| a.wrapping_mul(31).wrapping_add(b as usize));
                        let mut v = vec![0.0; DIMS];
                        v[h % DIMS] = 1.0;
                        v[h.wrapping_mul(7) % DIMS] = 0.5;
                        v
                    }
                })
                .collect())
        }
    }

    #[test]
    fn pipeline_ranks_with_fake_embedder() {
        let dir = tempfile::tempdir().unwrap();
        corpus::generate(dir.path()).unwrap();
        let idx = corpus::index();
        let n = idx.files.len();
        let mut fake = Fake { next: 0 };
        let store = build_store(dir.path(), &idx, &mut fake, "test", false).unwrap();
        assert_eq!(store.stats().unwrap().done as usize, n);

        // Images come first in arrival order (all images, then all notes); map vector -> file.
        let order: Vec<&str> = idx
            .files
            .iter()
            .filter(|f| f.kind == "image")
            .chain(idx.files.iter().filter(|f| f.kind == "text"))
            .map(|f| f.name.as_str())
            .collect();
        let cases = vec![
            Case {
                q: "k:0".into(),
                expect: vec![order[0].into()],
                tag: "a".into(),
            },
            Case {
                q: format!("k:{}", n - 1),
                expect: vec![order[n - 1].into()],
                tag: "b".into(),
            },
            Case {
                q: "k:3".into(),
                expect: vec!["nope*".into()],
                tag: "c".into(),
            },
        ];
        let res = run_cases(&store, &cases, &mut fake, n, false).unwrap();
        assert_eq!(res[0].rank, Some(1));
        assert_eq!(res[1].rank, Some(1));
        assert_eq!(res[2].rank, None);
        assert_eq!(res[0].top[0], order[0]);
    }

    #[test]
    fn model_files_follow_manifest() {
        let (m, p) = model_files(Path::new("M")).unwrap();
        assert!(m.ends_with("embeddinggemma-2-Q8_0.gguf"));
        assert!(p.ends_with("mmproj-embeddinggemma-2-Q8_0.gguf"));
    }

    #[test]
    fn code_cases_find_their_file_by_words_alone_without_a_model() {
        let d = tempfile::tempdir().unwrap();
        corpus::generate_code(d.path()).unwrap();
        let corpus = corpus::load_index(d.path()).unwrap();
        assert!(corpus.files.iter().all(|f| f.kind == "code"));
        let mut emb = Fake { next: 0 };
        let idx = build_code_store(d.path(), &corpus, &mut emb, "test").unwrap();
        let cases = corpus::code_cases();
        let results = run_code_cases(&idx, &cases, &mut emb, corpus.files.len()).unwrap();
        assert_eq!(results.len(), cases.len());
        let rank_of = |q: &str| results.iter().find(|r| r.q == q).and_then(|r| r.rank);
        // Identifier lookups must land on their file through bm25 + coverage alone — no
        // model in the loop, the one-hot vectors never pass the meaning gate. Names unique
        // to their file rank first;  ties with the architecture doc that
        // mentions it (equal coverage, resolved by path) and may rank second.
        for q in [
            "PaymentDeclined",
            "NewLRU",
            "useDebouncedQuery",
            "fit_inside",
        ] {
            assert_eq!(rank_of(q), Some(1), "{q} must rank first by words");
        }
        assert!(
            matches!(rank_of("retry_with_backoff"), Some(1 | 2)),
            "doc mention may tie: {:?}",
            rank_of("retry_with_backoff")
        );
        // The line-range expectation of an identifier case must overlap the definition.
        let tok = results.iter().find(|r| r.q == "fit_inside").unwrap();
        assert_eq!(tok.rank, Some(1));
        assert!(tok.top[0].ends_with("src/thumbs.rs"));
    }

    #[test]
    fn meta_matches_app_constants() {
        let m = index_meta("b1");
        assert_eq!(
            (
                m.model_id.as_str(),
                m.quant.as_str(),
                m.image_budget,
                m.dims
            ),
            ("embeddinggemma-2", "Q8_0", 280, 768)
        );
    }
}
