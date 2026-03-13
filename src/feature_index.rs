/// Port of complete-crate/src/crateFeatureService.ts
///
/// Fetches sparse crates.io index entries and resolves feature lists for a
/// given crate + Cargo version requirement.
use serde::Deserialize;
use std::collections::HashMap;
use tokio::sync::RwLock;

#[derive(Deserialize, Clone)]
struct SparseRecord {
    vers: String,
    #[serde(default)]
    yanked: bool,
    #[serde(default)]
    features: HashMap<String, Vec<String>>,
    #[serde(default)]
    features2: Option<HashMap<String, Vec<String>>>,
}

pub struct FeatureIndex {
    client: reqwest::Client,
    /// `None` means a previous fetch failed (don't retry immediately).
    cache: RwLock<HashMap<String, Option<Vec<SparseRecord>>>>,
}

impl FeatureIndex {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// Returns sorted feature names for the highest non-yanked version of
    /// `crate_name` that satisfies `version_req`.  Returns `None` on any error.
    pub async fn get_features(&self, crate_name: &str, version_req: &str) -> Option<Vec<String>> {
        let name = crate_name.trim().to_lowercase();
        if name.is_empty() { return None; }

        // Fast path: already cached
        {
            let cache = self.cache.read().await;
            if let Some(opt) = cache.get(&name) {
                return opt.as_ref().and_then(|r| resolve_features(r, version_req));
            }
        }

        // Fetch from crates.io sparse index
        let records = self.fetch_records(&name).await.ok();

        // Store result (even None, so we don't hammer the network)
        self.cache.write().await.insert(name.clone(), records.clone());

        records.as_ref().and_then(|r| resolve_features(r, version_req))
    }

    async fn fetch_records(&self, crate_name: &str) -> Result<Vec<SparseRecord>, String> {
        let url = format!("https://index.crates.io/{}", sparse_path(crate_name));
        let text = self
            .client
            .get(&url)
            .header("User-Agent", "cratelite-lsp/0.1.0")
            .send()
            .await
            .map_err(|e| e.to_string())?
            .text()
            .await
            .map_err(|e| e.to_string())?;

        let records = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<SparseRecord>(l).ok())
            .collect();
        Ok(records)
    }
}

// ── helpers ────────────────────────────────────────────────────────────────

/// Compute the relative URL path for a crate in the sparse index.
/// Mirrors `getSparseIndexPath` from TypeScript.
fn sparse_path(name: &str) -> String {
    match name.len() {
        0 => String::new(),
        1 => format!("1/{name}"),
        2 => format!("2/{name}"),
        3 => format!("3/{}/{name}", &name[..1]),
        _ => format!("{}/{}/{name}", &name[..2], &name[2..4]),
    }
}

/// Find the highest non-yanked version that satisfies `version_req` and
/// return its feature names.
fn resolve_features(records: &[SparseRecord], version_req: &str) -> Option<Vec<String>> {
    // The `semver` crate understands Cargo's version-requirement syntax natively.
    let req = semver::VersionReq::parse(version_req.trim()).ok()?;

    let record = records
        .iter()
        .filter(|r| !r.yanked)
        .filter_map(|r| semver::Version::parse(&r.vers).ok().map(|v| (v, r)))
        .filter(|(v, _)| req.matches(v))
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, r)| r)?;

    let mut features: Vec<String> = record.features.keys().cloned().collect();
    if let Some(f2) = &record.features2 {
        for key in f2.keys() {
            if !features.contains(key) {
                features.push(key.clone());
            }
        }
    }
    features.sort();
    Some(features)
}
