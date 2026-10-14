//! The live path: corpus -> (same image preprocessing as the app) -> embeddings -> in-memory
//! `Store` -> vector search per case. The embedder is a trait so the pipeline is unit-testable.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::json;
use winseek_llama::embed::truncate_normalize;
use winseek_llama::install::find_server_exe;
use winseek_llama::manifest::ModelRole;
use winseek_llama::prep::prepare_jpeg;
use winseek_llama::server::{build_args, Role, SpawnConfig};
use winseek_llama::{sha256_file, sha256_hex, EmbedClient, LlamaServer, Manifest, ModelProfile};
use winseek_store::{ChunkVec, FileKey, IndexMeta, Store, VecKind};

use crate::corpus::{CorpusIndex, load_index};
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
    fn queries(&mut self, queries: &[&str]) -> Result<Vec<Vec<f32>>, String>;
}

impl Embed for EmbedClient {
    fn images(&mut self, jpegs: &[&[u8]]) -> Result<Vec<Vec<f32>>, String> {
        self.embed_images_jpeg(jpegs).map_err(|e| e.to_string())
    }
    fn documents(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>, String> {
        self.embed_documents(texts).map_err(|e| e.to_string())
    }
    fn queries(&mut self, queries: &[&str]) -> Result<Vec<Vec<f32>>, String> {
        self.embed_queries(queries).map_err(|e| e.to_string())
    }
}

fn finish(raw: Vec<Vec<f32>>) -> Result<Vec<Vec<f32>>, String> {
    raw.iter().map(|v| truncate_normalize(v, DIMS).map_err(|e| e.to_string())).collect()
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

/// Embed every corpus file into a fresh in-memory store. Returns the store.
pub fn build_store(dir: &Path, corpus: &CorpusIndex, emb: &mut dyn Embed, llama_build: &str) -> Result<Store, String> {
    let mut store = Store::open_in_memory(&index_meta(llama_build)).map_err(|e| e.to_string())?;
    let mut images: Vec<(usize, Vec<u8>, String)> = Vec::new(); // (file no, jpeg, hash)
    let mut texts: Vec<(usize, String, String)> = Vec::new();
    let mut ids = Vec::new();
    for (i, f) in corpus.files.iter().enumerate() {
        let path = dir.join(&f.name);
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let hash = sha256_hex(&bytes);
        let id = store
            .upsert_file(&FileKey { vol_guid: "{eval}".into(), frn: i as u64 }, &path.to_string_lossy(), bytes.len() as u64, 0, &f.kind)
            .map_err(|e| e.to_string())?;
        ids.push((id, hash.clone()));
        match f.kind.as_str() {
            "image" => images.push((i, prepare_jpeg(&bytes).map_err(|e| format!("{}: {e}", f.name))?, hash)),
            "text" => texts.push((i, String::from_utf8_lossy(&bytes).into_owned(), hash)),
            other => return Err(format!("{}: unknown kind {other}", f.name)),
        }
    }
    for batch in images.chunks(IMAGE_BATCH) {
        let refs: Vec<&[u8]> = batch.iter().map(|(_, j, _)| j.as_slice()).collect();
        let vecs = finish(emb.images(&refs)?)?;
        if vecs.len() != batch.len() {
            return Err(format!("expected {} image vectors, got {}", batch.len(), vecs.len()));
        }
        for ((i, _, hash), vec) in batch.iter().zip(vecs) {
            let chunk = ChunkVec { chunk: 0, kind: VecKind::Image, locator: None, vec };
            store.complete(ids[*i].0, hash, &[chunk], &[]).map_err(|e| e.to_string())?;
        }
    }
    for batch in texts.chunks(IMAGE_BATCH) {
        let refs: Vec<&str> = batch.iter().map(|(_, t, _)| t.as_str()).collect();
        let vecs = finish(emb.documents(&refs)?)?;
        if vecs.len() != batch.len() {
            return Err(format!("expected {} text vectors, got {}", batch.len(), vecs.len()));
        }
        for ((i, body, hash), vec) in batch.iter().zip(vecs) {
            let chunk = ChunkVec { chunk: 0, kind: VecKind::Text, locator: None, vec };
            store.complete(ids[*i].0, hash, &[chunk], &[(0, body.clone())]).map_err(|e| e.to_string())?;
        }
    }
    Ok(store)
}

fn file_name_of(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Run every case against the store (query -> embed -> `search_vectors` over all files).
pub fn run_cases(store: &Store, cases: &[Case], emb: &mut dyn Embed, n_files: usize) -> Result<Vec<CaseResult>, String> {
    let mut out = Vec::with_capacity(cases.len());
    for c in cases {
        let q = finish(emb.queries(&[c.q.as_str()])?)?.into_iter().next().ok_or("empty query embedding")?;
        let hits = store.search_vectors(&q, n_files.max(5), None).map_err(|e| e.to_string())?;
        let ranked: Vec<String> = hits.iter().map(|h| file_name_of(&h.path)).collect();
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

/// Embedding-model file names from the pinned manifest (same entry the app loads).
pub fn model_files(models_dir: &Path) -> Result<(PathBuf, PathBuf), String> {
    let man = Manifest::embedded();
    let entry = man.model(MODEL_ID, ModelProfile::Default).ok_or("manifest has no embeddinggemma-2 entry")?;
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
}

fn version_of(exe: &Path) -> String {
    Command::new(exe)
        .arg("--version")
        .current_dir(exe.parent().unwrap_or(Path::new(".")))
        .output()
        .map(|o| String::from_utf8_lossy(&[o.stdout, o.stderr].concat()).lines().find(|l| l.contains("version")).unwrap_or("").trim().to_string())
        .unwrap_or_default()
}

/// Full live run. Spawns llama-server (unless `url` is given) and drops it at the end.
pub fn run(opts: &RunOptions) -> Result<Report, String> {
    let corpus = load_index(&opts.corpus)?;
    let cases_text = std::fs::read_to_string(&opts.cases).map_err(|e| format!("{}: {e}", opts.cases.display()))?;
    let cases = crate::metrics::parse_cases(&cases_text)?;
    let manifest = Manifest::embedded();

    let (model, mmproj) = model_files(&opts.models)?;
    let mut meta = json!({
        "manifest_build": manifest.llama.build,
        "dims": DIMS,
        "quant": QUANT,
        "image_budget": IMAGE_BUDGET,
        "corpus_files": corpus.files.len(),
        "cases": cases.len(),
    });

    // Keep the server alive until the end of the run (Drop kills it).
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
            let exe = find_server_exe(&opts.llama).ok_or_else(|| format!("no llama-server under {}", opts.llama.display()))?;
            eprintln!("hashing model files...");
            let log = std::env::temp_dir().join("winseek-eval-llama.log");
            let cfg = SpawnConfig { role: Role::EmbedIndex, log_file: Some(log.clone()), ..Default::default() };
            eprintln!("spawning {} (log: {})", exe.display(), log.display());
            let server = LlamaServer::spawn_with(&exe, &model, Some(&mmproj), &[], &cfg).map_err(|e| e.to_string())?;
            // The exact argument list the preset produced, with the throwaway key redacted.
            let args = build_args(&model, Some(&mmproj), server.port(), "***", Role::EmbedIndex, Default::default(), &[]);
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
    meta["unix_time"] = json!(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));

    let mut client = EmbedClient::new(&base_url, &key);
    match client.vision_enabled() {
        Ok(true) => {}
        Ok(false) => return Err("llama-server has no vision (mmproj not loaded)".into()),
        Err(e) => return Err(format!("/props: {e}")),
    }
    let started = std::time::Instant::now();
    eprintln!("embedding {} corpus files...", corpus.files.len());
    let store = build_store(&opts.corpus, &corpus, &mut client, &manifest.llama.build)?;
    meta["index_secs"] = json!((started.elapsed().as_secs_f64() * 100.0).round() / 100.0);
    eprintln!("running {} cases...", cases.len());
    let results = run_cases(&store, &cases, &mut client, corpus.files.len())?;
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
            Ok(jpegs.iter().map(|_| {
                self.next += 1;
                onehot(self.next - 1)
            }).collect())
        }
        fn documents(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>, String> {
            Ok(texts.iter().map(|_| {
                self.next += 1;
                onehot(self.next - 1)
            }).collect())
        }
        fn queries(&mut self, queries: &[&str]) -> Result<Vec<Vec<f32>>, String> {
            Ok(queries.iter().map(|q| onehot(q.strip_prefix("k:").unwrap().parse().unwrap())).collect())
        }
    }

    #[test]
    fn pipeline_ranks_with_fake_embedder() {
        let dir = tempfile::tempdir().unwrap();
        corpus::generate(dir.path()).unwrap();
        let idx = corpus::index();
        let n = idx.files.len();
        let mut fake = Fake { next: 0 };
        let store = build_store(dir.path(), &idx, &mut fake, "test").unwrap();
        assert_eq!(store.stats().unwrap().done as usize, n);

        // Images come first in arrival order (all images, then all notes); map vector -> file.
        let order: Vec<&str> = idx.files.iter().filter(|f| f.kind == "image").chain(idx.files.iter().filter(|f| f.kind == "text")).map(|f| f.name.as_str()).collect();
        let cases = vec![
            Case { q: "k:0".into(), expect: vec![order[0].into()], tag: "a".into() },
            Case { q: format!("k:{}", n - 1), expect: vec![order[n - 1].into()], tag: "b".into() },
            Case { q: "k:3".into(), expect: vec!["nope*".into()], tag: "c".into() },
        ];
        let res = run_cases(&store, &cases, &mut fake, n).unwrap();
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
    fn meta_matches_app_constants() {
        let m = index_meta("b1");
        assert_eq!((m.model_id.as_str(), m.quant.as_str(), m.image_budget, m.dims), ("embeddinggemma-2", "Q8_0", 280, 768));
    }
}
