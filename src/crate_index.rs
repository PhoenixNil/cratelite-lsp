/// Port of complete-crate/src/crateIndex.ts
///
/// Downloads and maintains a prefix-bucketed index of crate names + latest
/// versions.  The index is persisted to disk and refreshed every 24 hours.
use flate2::read::GzDecoder;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{collections::HashMap, io::Read, path::PathBuf, sync::Arc};
use tokio::sync::RwLock;

const INDEX_URL: &str =
    "https://github.com/PhoenixNil/Autoupdate-cratelite/releases/download/latest/crates-index.txt.gz";
const CACHE_TTL_SECS: u64 = 24 * 60 * 60;

#[derive(Clone)]
pub struct CrateEntry {
    pub name: String,
    pub version: String,
    #[allow(dead_code)]
    pub rank: usize,
}

pub struct CrateIndex {
    buckets: RwLock<HashMap<String, Vec<CrateEntry>>>,
    loaded: RwLock<bool>,
}

// ── cache paths ────────────────────────────────────────────────────────────

fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("cratelite")
}

fn cache_file() -> PathBuf {
    cache_dir().join("crates-index.txt")
}

fn meta_file() -> PathBuf {
    cache_dir().join("index-meta.json")
}

fn is_cache_expired() -> bool {
    let path = meta_file();
    let Ok(content) = std::fs::read_to_string(&path) else {
        return true;
    };
    let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) else {
        return true;
    };
    let Some(ts) = val["lastUpdate"].as_u64() else {
        return true;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    now.saturating_sub(ts) > CACHE_TTL_SECS
}

fn save_meta() {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let json = serde_json::json!({ "lastUpdate": now });
    let _ = std::fs::write(meta_file(), json.to_string());
}

// ── implementation ─────────────────────────────────────────────────────────

impl CrateIndex {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            buckets: RwLock::new(HashMap::new()),
            loaded: RwLock::new(false),
        })
    }

    /// Load from cache (if present), then refresh in the background when stale.
    pub async fn initialize(self: Arc<Self>) {
        let _ = std::fs::create_dir_all(cache_dir());
        let cache_path = cache_file();

        if cache_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&cache_path) {
                self.build_index(&content).await;
            }
            if is_cache_expired() {
                let this = self.clone();
                tokio::spawn(async move {
                    if let Err(e) = this.download_and_build().await {
                        eprintln!("cratelite-lsp: background index refresh failed: {e}");
                    }
                });
            }
        } else {
            // First run — must download synchronously so completions are available
            if let Err(e) = self.download_and_build().await {
                eprintln!("cratelite-lsp: initial index download failed: {e}");
            }
        }
    }

    async fn download_and_build(&self) -> Result<(), String> {
        let data = reqwest::Client::new()
            .get(INDEX_URL)
            .header("User-Agent", "cratelite-lsp/0.1.0")
            .send()
            .await
            .map_err(|e| e.to_string())?
            .bytes()
            .await
            .map_err(|e| e.to_string())?;

        let mut decoder = GzDecoder::new(&data[..]);
        let mut content = String::new();
        decoder
            .read_to_string(&mut content)
            .map_err(|e| e.to_string())?;

        std::fs::write(cache_file(), &content).map_err(|e| e.to_string())?;
        save_meta();
        self.build_index(&content).await;
        Ok(())
    }

    async fn build_index(&self, content: &str) {
        let mut map: HashMap<String, Vec<CrateEntry>> = HashMap::new();

        for (rank, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let (name, version) = match line.find(' ') {
                Some(i) => (&line[..i], &line[i + 1..]),
                None => (line, "0.0.0"),
            };

            if name.len() < 2 {
                continue;
            }

            let prefix = name[..2].to_lowercase();
            map.entry(prefix).or_default().push(CrateEntry {
                name: name.to_string(),
                version: version.to_string(),
                rank,
            });
        }

        *self.buckets.write().await = map;
        *self.loaded.write().await = true;
    }

    // ── query methods ──────────────────────────────────────────────────────

    pub async fn search(&self, query: &str, max_results: usize) -> Vec<CrateEntry> {
        if !*self.loaded.read().await || query.len() < 2 {
            return vec![];
        }
        let q = query.to_lowercase();
        let prefix = &q[..2];
        let buckets = self.buckets.read().await;
        let Some(bucket) = buckets.get(prefix) else {
            return vec![];
        };

        let mut results = Vec::new();
        for entry in bucket {
            if entry.name.to_lowercase().starts_with(q.as_str()) {
                results.push(entry.clone());
                if results.len() >= max_results {
                    break;
                }
            }
        }
        results
    }

    pub async fn get_latest_version(&self, crate_name: &str) -> Option<String> {
        if !*self.loaded.read().await {
            return None;
        }
        let q = crate_name.to_lowercase();
        let buckets = self.buckets.read().await;
        if q.len() < 2 {
            return None;
        }
        let bucket = buckets.get(&q[..2])?;
        bucket
            .iter()
            .find(|e| e.name.to_lowercase() == q)
            .map(|e| e.version.clone())
    }

    #[allow(dead_code)]
    pub async fn is_loaded(&self) -> bool {
        *self.loaded.read().await
    }
}
