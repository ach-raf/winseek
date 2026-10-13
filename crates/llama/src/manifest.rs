//! Pinned manifest: llama.cpp build + model files.

use serde::{Deserialize, Serialize};

use crate::backend::Backend;
use crate::{Error, Result};

pub const DEFAULT_MANIFEST_JSON: &str = include_str!("../manifest.json");

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub llama: LlamaManifest,
    pub models: Vec<ModelEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlamaManifest {
    /// e.g. "b11279"
    pub build: String,
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Asset {
    pub backend: Backend,
    pub url: String,
    pub sha256: String,
    /// Zip size in bytes (for the setup plan and progress totals); `None` when not pinned.
    #[serde(default)]
    pub size: Option<u64>,
    /// Additional zips extracted into the same dir (cudart for CUDA).
    #[serde(default)]
    pub extra: Vec<UrlHash>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UrlHash {
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    Model,
    Mmproj,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelProfile {
    Default,
    Quality,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelFile {
    pub role: ModelRole,
    pub url: String,
    pub sha256: String,
    pub size: u64,
    /// Alternative download URLs (the pinned sha256 makes any mirror safe).
    #[serde(default)]
    pub mirrors: Vec<String>,
}

impl ModelFile {
    /// File name taken from the URL's last path segment.
    pub fn file_name(&self) -> &str {
        self.url.rsplit('/').next().unwrap_or(&self.url)
    }

    /// `[url, mirrors...]`, each rewritten through `WINSEEK_HF_ENDPOINT` if set.
    pub fn urls(&self) -> Vec<String> {
        let ep = std::env::var("WINSEEK_HF_ENDPOINT").ok();
        self.urls_with(ep.as_deref())
    }

    /// Same as [`urls`](Self::urls) with an explicit endpoint (pure).
    pub fn urls_with(&self, endpoint: Option<&str>) -> Vec<String> {
        std::iter::once(&self.url).chain(self.mirrors.iter()).map(|u| crate::fetch::rewrite_hf(u, endpoint)).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    pub files: Vec<ModelFile>,
    pub profile: ModelProfile,
}

impl Manifest {
    pub fn parse(json: &str) -> Result<Manifest> {
        serde_json::from_str(json).map_err(|e| Error::Manifest(e.to_string()))
    }

    /// The manifest embedded in the crate.
    pub fn embedded() -> Manifest {
        Self::parse(DEFAULT_MANIFEST_JSON).expect("embedded manifest.json is valid")
    }

    pub fn asset(&self, backend: Backend) -> Option<&Asset> {
        self.llama.assets.iter().find(|a| a.backend == backend)
    }

    pub fn model(&self, id: &str, profile: ModelProfile) -> Option<&ModelEntry> {
        self.models.iter().find(|m| m.id == id && m.profile == profile)
    }
}

/// True for the `TODO-pin-in-CI` placeholder or anything that is not 64 hex chars.
pub fn is_unpinned(sha256: &str) -> bool {
    !(sha256.len() == 64 && sha256.bytes().all(|b| b.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_parses() {
        let m = Manifest::embedded();
        assert!(m.llama.build.starts_with('b'));
        for b in [Backend::Cuda12, Backend::Vulkan, Backend::Cpu] {
            let a = m.asset(b).unwrap();
            assert!(a.url.contains(&m.llama.build) && a.url.ends_with(".zip"), "{}", a.url);
        }
        assert_eq!(m.asset(Backend::Cuda12).unwrap().extra.len(), 1);
        for a in &m.llama.assets {
            assert!(a.size.is_some_and(|s| s > 0), "{} has no size", a.url);
            for e in &a.extra {
                assert!(e.size.is_some_and(|s| s > 0), "{} has no size", e.url);
            }
        }
        let q4 = m.model("infinity-parser2-flash", ModelProfile::Default).unwrap();
        assert_eq!(q4.files.len(), 2);
        assert_eq!(q4.files[0].file_name(), "Infinity-Parser2-Flash.Q4_K_M.gguf");
        assert_eq!(q4.files[1].role, ModelRole::Mmproj);
        let q8 = m.model("infinity-parser2-flash", ModelProfile::Quality).unwrap();
        assert!(q8.files[0].url.ends_with("Q8_0.gguf"));
        assert!(m.model("embeddinggemma-2", ModelProfile::Default).is_some());
        assert!(m.model("nope", ModelProfile::Default).is_none());
    }

    #[test]
    fn parse_minimal_and_reject_bad() {
        let j = r#"{"llama":{"build":"b1","assets":[{"backend":"cpu","url":"u","sha256":"s"}]},"models":[]}"#;
        let m = Manifest::parse(j).unwrap();
        assert!(m.asset(Backend::Cpu).unwrap().extra.is_empty());
        assert!(Manifest::parse(r#"{"llama":{"build":"b1","assets":[{"backend":"tpu","url":"u","sha256":"s"}]},"models":[]}"#).is_err());
        assert!(Manifest::parse("{").is_err());
    }

    #[test]
    fn mirrors_optional_and_urls() {
        let m = Manifest::embedded();
        let f = &m.model("embeddinggemma-2", ModelProfile::Default).unwrap().files[0];
        assert!(f.mirrors.is_empty());
        assert_eq!(f.urls_with(None), vec![f.url.clone()]);
        let f2 = ModelFile {
            role: ModelRole::Model,
            url: "https://huggingface.co/a/b/resolve/main/x.gguf".into(),
            sha256: "s".into(),
            size: 1,
            mirrors: vec!["https://mirror.example/x.gguf".into(), "https://huggingface.co/c/x.gguf".into()],
        };
        assert_eq!(
            f2.urls_with(Some("https://hf-mirror.com/")),
            [
                "https://hf-mirror.com/a/b/resolve/main/x.gguf",
                "https://mirror.example/x.gguf",
                "https://hf-mirror.com/c/x.gguf"
            ]
        );
        let j: ModelFile = serde_json::from_str(r#"{"role":"model","url":"u","sha256":"s","size":1,"mirrors":["m"]}"#).unwrap();
        assert_eq!(j.mirrors, ["m"]);
    }

    #[test]
    fn unpinned_detection() {
        assert!(is_unpinned("TODO-pin-in-CI"));
        assert!(!is_unpinned(&"a".repeat(64)));
    }
}
