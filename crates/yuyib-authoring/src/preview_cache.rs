//! Policy-aware preview cache keys and a bounded in-process cache.
//!
//! [`PreviewCachePolicy`] flags decide which request dimensions participate in
//! the key. Entries are single-owner: a hit [`PreviewCache::take`] moves the
//! artifact out exactly once (matching [`PreviewArtifact`] handoff).

use std::{
    any::Any,
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
};

use serde_json::Value;

use crate::{
    AssetGuid, CapabilityId, ContentHash, PreviewArtifact, PreviewCachePolicy,
    PreviewMaterialOverride, PreviewOverlay, PreviewRequest, PreviewSelection,
};

/// Deterministic cache key derived from a [`PreviewRequest`] under a policy.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PreviewCacheKey {
    asset: AssetGuid,
    /// Present only when `invalidate_on_content_hash_change` is enabled.
    content_hash: Option<Option<ContentHash>>,
    /// Present only when `invalidate_on_import_settings_change` is enabled.
    import_settings: Option<ImportSettingsFingerprint>,
    selection: Option<SelectionFingerprint>,
    overlays: BTreeSet<PreviewOverlay>,
    material_override: Option<String>,
    render_preset: Option<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ImportSettingsFingerprint {
    schema: String,
    version: u32,
    payload: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum SelectionFingerprint {
    Mesh(u32),
    Material(u32),
    AnimationClip(String),
}

impl PreviewCacheKey {
    /// Builds a key, applying invalidation flags from `policy`.
    #[must_use]
    pub fn from_request(request: &PreviewRequest, policy: PreviewCachePolicy) -> Self {
        Self {
            asset: request.asset,
            content_hash: policy
                .invalidate_on_content_hash_change
                .then(|| request.content_hash.clone()),
            import_settings: policy
                .invalidate_on_import_settings_change
                .then(|| ImportSettingsFingerprint {
                    schema: request.import_settings_schema.as_str().to_owned(),
                    version: request.import_settings_version.get(),
                    payload: fingerprint_json(&request.import_settings),
                }),
            selection: request.selection.as_ref().map(|selection| match selection {
                PreviewSelection::Mesh(index) => SelectionFingerprint::Mesh(*index),
                PreviewSelection::Material(index) => SelectionFingerprint::Material(*index),
                PreviewSelection::AnimationClip(name) => {
                    SelectionFingerprint::AnimationClip(name.clone())
                }
            }),
            overlays: request.overlays.clone(),
            material_override: request
                .material_override
                .as_ref()
                .map(fingerprint_material_override),
            render_preset: request
                .render_preset
                .as_ref()
                .map(|preset| preset.id.as_str().to_owned()),
        }
    }

    /// Persistent asset identity for this key.
    #[must_use]
    pub const fn asset(&self) -> AssetGuid {
        self.asset
    }
}

/// Bounded preview result cache with LRU eviction.
pub struct PreviewCache {
    policy: PreviewCachePolicy,
    entries: BTreeMap<PreviewCacheKey, CacheEntry>,
    order: VecDeque<PreviewCacheKey>,
    total_bytes: u64,
}

struct CacheEntry {
    asset: AssetGuid,
    capability: CapabilityId,
    cpu_bytes: u64,
    gpu_bytes: u64,
    payload: Box<dyn Any + Send>,
}

/// Failure while inserting into a [`PreviewCache`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PreviewCacheError {
    /// Artifact CPU+GPU bytes exceed the entire cache budget alone.
    ExceedsBudget {
        /// Accounted bytes required by the artifact.
        required: u64,
        /// Cache `max_bytes` from policy.
        max_bytes: u64,
    },
}

impl fmt::Display for PreviewCacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExceedsBudget {
                required,
                max_bytes,
            } => write!(
                formatter,
                "preview artifact requires {required} bytes but cache max_bytes is {max_bytes}"
            ),
        }
    }
}

impl std::error::Error for PreviewCacheError {}

impl PreviewCache {
    /// Creates an empty cache governed by `policy`.
    #[must_use]
    pub fn new(policy: PreviewCachePolicy) -> Self {
        Self {
            policy,
            entries: BTreeMap::new(),
            order: VecDeque::new(),
            total_bytes: 0,
        }
    }

    /// Returns the governing policy.
    #[must_use]
    pub const fn policy(&self) -> PreviewCachePolicy {
        self.policy
    }

    /// Number of retained entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Sum of accounted CPU+GPU bytes across entries.
    #[must_use]
    pub const fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// Whether `key` is currently retained.
    #[must_use]
    pub fn contains(&self, key: &PreviewCacheKey) -> bool {
        self.entries.contains_key(key)
    }

    /// Inserts `artifact` under `key`, evicting LRU entries as needed.
    ///
    /// Replacing an existing key removes the previous payload first.
    ///
    /// # Errors
    ///
    /// Returns [`PreviewCacheError::ExceedsBudget`] when a single artifact cannot
    /// fit even after emptying the cache.
    pub fn insert(
        &mut self,
        key: PreviewCacheKey,
        artifact: PreviewArtifact,
    ) -> Result<(), PreviewCacheError> {
        let required = artifact.cpu_bytes().saturating_add(artifact.gpu_bytes());
        let max_bytes = self.policy.max_bytes.get();
        if required > max_bytes {
            return Err(PreviewCacheError::ExceedsBudget {
                required,
                max_bytes,
            });
        }

        if self.entries.contains_key(&key) {
            self.remove_key(&key);
        }

        while !self.entries.is_empty()
            && (self.entries.len() as u32 >= self.policy.max_entries.get()
                || self.total_bytes.saturating_add(required) > max_bytes)
        {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.remove_key(&oldest);
        }

        // Entry count can still block when max_entries == 1 and we just cleared.
        while self.entries.len() as u32 >= self.policy.max_entries.get() {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.remove_key(&oldest);
        }

        let asset = artifact.asset();
        let capability = artifact.capability().clone();
        let cpu_bytes = artifact.cpu_bytes();
        let gpu_bytes = artifact.gpu_bytes();
        let payload = artifact.into_any_payload();

        self.total_bytes = self.total_bytes.saturating_add(required);
        self.order.push_back(key.clone());
        self.entries.insert(
            key,
            CacheEntry {
                asset,
                capability,
                cpu_bytes,
                gpu_bytes,
                payload,
            },
        );
        Ok(())
    }

    /// Removes and returns the artifact for `key` (cache hit + consume).
    pub fn take(&mut self, key: &PreviewCacheKey) -> Option<PreviewArtifact> {
        let entry = self.remove_key(key)?;
        Some(PreviewArtifact::from_parts(
            entry.asset,
            entry.capability,
            entry.cpu_bytes,
            entry.gpu_bytes,
            entry.payload,
        ))
    }

    /// Drops every entry whose key asset equals `guid`.
    pub fn invalidate_asset(&mut self, guid: AssetGuid) {
        let doomed: Vec<_> = self
            .entries
            .keys()
            .filter(|key| key.asset() == guid)
            .cloned()
            .collect();
        for key in doomed {
            self.remove_key(&key);
        }
    }

    /// Drops entries for any of `guids` (selective dependent invalidation).
    pub fn invalidate_assets(&mut self, guids: &[AssetGuid]) {
        let set: BTreeSet<_> = guids.iter().copied().collect();
        if set.is_empty() {
            return;
        }
        let doomed: Vec<_> = self
            .entries
            .keys()
            .filter(|key| set.contains(&key.asset()))
            .cloned()
            .collect();
        for key in doomed {
            self.remove_key(&key);
        }
    }

    fn remove_key(&mut self, key: &PreviewCacheKey) -> Option<CacheEntry> {
        let entry = self.entries.remove(key)?;
        self.total_bytes = self
            .total_bytes
            .saturating_sub(entry.cpu_bytes.saturating_add(entry.gpu_bytes));
        self.order.retain(|existing| existing != key);
        Some(entry)
    }
}

fn fingerprint_material_override(override_: &PreviewMaterialOverride) -> String {
    let mut parts = Vec::with_capacity(override_.parameters.len());
    for (name, value) in &override_.parameters {
        parts.push(format!("{name}:{}", fingerprint_json(value)));
    }
    parts.join("|")
}

fn fingerprint_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => format!("\"{text}\""),
        Value::Array(items) => {
            let inner = items
                .iter()
                .map(fingerprint_json)
                .collect::<Vec<_>>()
                .join(",");
            format!("[{inner}]")
        }
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();
            let inner = keys
                .into_iter()
                .map(|key| format!("{key}:{}", fingerprint_json(&map[key])))
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{inner}}}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CapabilityId, ImportSettingsSchemaId, PreviewCancellation, SchemaVersion};
    use serde_json::json;
    use std::num::{NonZeroU32, NonZeroU64};

    fn policy(invalidate_hash: bool, invalidate_settings: bool) -> PreviewCachePolicy {
        PreviewCachePolicy {
            max_entries: NonZeroU32::new(2).expect("entries"),
            max_bytes: NonZeroU64::new(1_000).expect("bytes"),
            invalidate_on_content_hash_change: invalidate_hash,
            invalidate_on_import_settings_change: invalidate_settings,
        }
    }

    fn request(
        asset: AssetGuid,
        hash: Option<&str>,
        settings: Value,
    ) -> PreviewRequest {
        PreviewRequest {
            asset,
            source: "assets/a.glb".to_owned(),
            content_hash: hash.map(|value| ContentHash::new(value).expect("hash")),
            import_settings_schema: ImportSettingsSchemaId::new("yuyib.gltf-import-settings")
                .expect("schema"),
            import_settings_version: SchemaVersion::new(1).expect("version"),
            import_settings: settings,
            selection: None,
            overlays: BTreeSet::new(),
            material_override: None,
            render_preset: None,
            cancellation: PreviewCancellation::default(),
        }
    }

    fn artifact(asset: AssetGuid, cpu: u64, token: u8) -> PreviewArtifact {
        let capability = CapabilityId::new("yuyib.gltf-preview").expect("cap");
        PreviewArtifact::new(asset, capability, cpu, 0, vec![token])
    }

    #[test]
    fn content_hash_change_misses_when_policy_requires_it() {
        let asset = AssetGuid::new();
        let policy = policy(true, false);
        let first = PreviewCacheKey::from_request(
            &request(asset, Some("blake3:aa"), json!({})),
            policy,
        );
        let second = PreviewCacheKey::from_request(
            &request(asset, Some("blake3:bb"), json!({})),
            policy,
        );
        assert_ne!(first, second);
    }

    #[test]
    fn content_hash_change_hits_when_policy_ignores_hash() {
        let asset = AssetGuid::new();
        let policy = policy(false, true);
        let first = PreviewCacheKey::from_request(
            &request(asset, Some("blake3:aa"), json!({})),
            policy,
        );
        let second = PreviewCacheKey::from_request(
            &request(asset, Some("blake3:bb"), json!({})),
            policy,
        );
        assert_eq!(first, second);
    }

    #[test]
    fn import_settings_change_misses_when_policy_requires_it() {
        let asset = AssetGuid::new();
        let policy = policy(false, true);
        let first = PreviewCacheKey::from_request(
            &request(asset, None, json!({"policy": "strict"})),
            policy,
        );
        let second = PreviewCacheKey::from_request(
            &request(asset, None, json!({"policy": "permissive"})),
            policy,
        );
        assert_ne!(first, second);
    }

    #[test]
    fn settings_fingerprint_is_key_order_independent() {
        let asset = AssetGuid::new();
        let policy = policy(false, true);
        let first = PreviewCacheKey::from_request(
            &request(asset, None, json!({"b": 1, "a": 2})),
            policy,
        );
        let second = PreviewCacheKey::from_request(
            &request(asset, None, json!({"a": 2, "b": 1})),
            policy,
        );
        assert_eq!(first, second);
    }

    #[test]
    fn hit_take_skips_redecode_and_consumes_entry() {
        let asset = AssetGuid::new();
        let policy = policy(true, true);
        let key = PreviewCacheKey::from_request(
            &request(asset, Some("blake3:aa"), json!({})),
            policy,
        );
        let mut cache = PreviewCache::new(policy);
        cache
            .insert(key.clone(), artifact(asset, 10, 7))
            .expect("insert");
        assert!(cache.contains(&key));
        let taken = cache.take(&key).expect("hit");
        assert_eq!(taken.downcast::<Vec<u8>>().expect("payload"), vec![7]);
        assert!(!cache.contains(&key));
        assert!(cache.take(&key).is_none());
    }

    #[test]
    fn eviction_respects_max_entries() {
        let policy = policy(true, true);
        let mut cache = PreviewCache::new(policy);
        let a = AssetGuid::new();
        let b = AssetGuid::new();
        let c = AssetGuid::new();
        let key_a = PreviewCacheKey::from_request(&request(a, Some("blake3:01"), json!({})), policy);
        let key_b = PreviewCacheKey::from_request(&request(b, Some("blake3:02"), json!({})), policy);
        let key_c = PreviewCacheKey::from_request(&request(c, Some("blake3:03"), json!({})), policy);
        cache.insert(key_a.clone(), artifact(a, 10, 1)).expect("a");
        cache.insert(key_b.clone(), artifact(b, 10, 2)).expect("b");
        cache.insert(key_c.clone(), artifact(c, 10, 3)).expect("c");
        assert_eq!(cache.len(), 2);
        assert!(!cache.contains(&key_a));
        assert!(cache.contains(&key_b));
        assert!(cache.contains(&key_c));
    }

    #[test]
    fn invalidate_assets_drops_only_declared_dependents() {
        let wide = PreviewCachePolicy {
            max_entries: NonZeroU32::new(8).expect("entries"),
            max_bytes: NonZeroU64::new(10_000).expect("bytes"),
            invalidate_on_content_hash_change: true,
            invalidate_on_import_settings_change: true,
        };
        let mut cache = PreviewCache::new(wide);
        let root = AssetGuid::new();
        let dependent = AssetGuid::new();
        let other = AssetGuid::new();
        let key_root =
            PreviewCacheKey::from_request(&request(root, Some("blake3:01"), json!({})), wide);
        let key_dep =
            PreviewCacheKey::from_request(&request(dependent, Some("blake3:02"), json!({})), wide);
        let key_other =
            PreviewCacheKey::from_request(&request(other, Some("blake3:03"), json!({})), wide);
        cache
            .insert(key_root.clone(), artifact(root, 10, 1))
            .expect("root");
        cache
            .insert(key_dep.clone(), artifact(dependent, 10, 2))
            .expect("dep");
        cache
            .insert(key_other.clone(), artifact(other, 10, 3))
            .expect("other");
        cache.invalidate_assets(&[dependent]);
        assert!(cache.contains(&key_root));
        assert!(!cache.contains(&key_dep));
        assert!(cache.contains(&key_other));
    }

    #[test]
    fn insert_rejects_artifact_larger_than_budget() {
        let policy = PreviewCachePolicy {
            max_entries: NonZeroU32::new(4).expect("entries"),
            max_bytes: NonZeroU64::new(50).expect("bytes"),
            invalidate_on_content_hash_change: true,
            invalidate_on_import_settings_change: true,
        };
        let mut cache = PreviewCache::new(policy);
        let asset = AssetGuid::new();
        let key = PreviewCacheKey::from_request(&request(asset, Some("blake3:01"), json!({})), policy);
        let error = cache
            .insert(key, artifact(asset, 80, 1))
            .expect_err("too large");
        assert_eq!(
            error,
            PreviewCacheError::ExceedsBudget {
                required: 80,
                max_bytes: 50
            }
        );
    }
}
