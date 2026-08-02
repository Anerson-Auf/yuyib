use std::{error::Error, fmt, num::NonZeroU32, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use uuid::Uuid;

/// Explains why a persisted identifier was rejected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StableIdError {
    value: String,
    reason: &'static str,
}

impl StableIdError {
    fn new(value: &str, reason: &'static str) -> Self {
        Self {
            value: value.to_owned(),
            reason,
        }
    }

    /// Returns the rejected value.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Returns a stable, human-readable validation reason.
    #[must_use]
    pub const fn reason(&self) -> &'static str {
        self.reason
    }
}

impl fmt::Display for StableIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid stable id {:?}: {}",
            self.value, self.reason
        )
    }
}

impl Error for StableIdError {}

fn validate_stable_id(value: &str) -> Result<(), StableIdError> {
    if value.len() > 191 {
        return Err(StableIdError::new(value, "must not exceed 191 bytes"));
    }
    let mut segments = value.split('.');
    let Some(first) = segments.next() else {
        return Err(StableIdError::new(
            value,
            "must contain at least two segments",
        ));
    };
    let Some(second) = segments.next() else {
        return Err(StableIdError::new(
            value,
            "must contain at least two segments",
        ));
    };
    validate_segment(value, first)?;
    validate_segment(value, second)?;
    for segment in segments {
        validate_segment(value, segment)?;
    }
    Ok(())
}

fn validate_segment(full_value: &str, segment: &str) -> Result<(), StableIdError> {
    if segment.is_empty() {
        return Err(StableIdError::new(full_value, "segments must not be empty"));
    }
    let bytes = segment.as_bytes();
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return Err(StableIdError::new(
            full_value,
            "segments must start with a lowercase ASCII letter or digit",
        ));
    }
    if !bytes[bytes.len() - 1].is_ascii_lowercase() && !bytes[bytes.len() - 1].is_ascii_digit() {
        return Err(StableIdError::new(
            full_value,
            "segments must end with a lowercase ASCII letter or digit",
        ));
    }
    if bytes
        .iter()
        .any(|byte| !byte.is_ascii_lowercase() && !byte.is_ascii_digit() && *byte != b'-')
    {
        return Err(StableIdError::new(
            full_value,
            "only lowercase ASCII letters, digits, hyphens, and dots are allowed",
        ));
    }
    Ok(())
}

macro_rules! stable_id {
    ($name:ident, $documentation:literal) => {
        #[doc = $documentation]
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);

        impl $name {
            /// Parses and validates a stable identifier.
            ///
            /// # Errors
            ///
            /// Returns [`StableIdError`] when the value is not a dot-separated,
            /// lowercase, portable identifier such as `yuyib.transform3d`.
            pub fn new(value: impl Into<String>) -> Result<Self, StableIdError> {
                let value = value.into();
                validate_stable_id(&value)?;
                Ok(Self(value))
            }

            /// Returns the canonical identifier text.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = StableIdError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(de::Error::custom)
            }
        }
    };
}

stable_id!(
    CapabilityId,
    "Stable identifier for one public runtime or authoring capability."
);
stable_id!(
    ComponentSchemaId,
    "Stable identifier for a persisted component schema."
);
stable_id!(
    ImportSettingsSchemaId,
    "Stable identifier for a persisted importer-settings schema."
);
stable_id!(SystemId, "Stable identifier for a runtime system.");
stable_id!(PluginId, "Stable identifier for an owning runtime plugin.");
stable_id!(ScheduleId, "Stable identifier for a runtime schedule.");

/// A non-zero persisted schema version.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct SchemaVersion(NonZeroU32);

impl SchemaVersion {
    /// First valid persisted schema version.
    pub const INITIAL: Self = Self(NonZeroU32::MIN);

    /// Creates a schema version, rejecting zero.
    ///
    /// # Errors
    ///
    /// Returns [`StableIdError`] when `value` is zero.
    pub fn new(value: u32) -> Result<Self, StableIdError> {
        NonZeroU32::new(value).map(Self).ok_or_else(|| {
            StableIdError::new(&value.to_string(), "schema versions must be non-zero")
        })
    }

    /// Returns the numeric schema version.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

impl<'de> Deserialize<'de> for SchemaVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u32::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

macro_rules! guid {
    ($name:ident, $documentation:literal) => {
        #[doc = $documentation]
        #[derive(
            Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Generates a new random persistent identity.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Wraps an existing UUID.
            #[must_use]
            pub const fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }

            /// Returns the underlying UUID.
            #[must_use]
            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(value).map(Self)
            }
        }
    };
}

guid!(
    EntityGuid,
    "Persistent authored entity identity, independent from runtime ECS entities."
);
guid!(
    AssetGuid,
    "Persistent asset identity that survives source renames and content changes."
);
guid!(
    ProjectGuid,
    "Persistent project identity, distinct from project paths and content hashes."
);
guid!(
    SceneGuid,
    "Persistent authored scene identity, independent from its path and contents."
);

/// A cache/invalidation hash, deliberately distinct from [`AssetGuid`].
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContentHash(String);

impl ContentHash {
    /// Creates an algorithm-qualified content hash such as `sha256:12ab`.
    ///
    /// # Errors
    ///
    /// Returns [`StableIdError`] if the algorithm or lowercase hexadecimal
    /// digest is malformed.
    pub fn new(value: impl Into<String>) -> Result<Self, StableIdError> {
        let value = value.into();
        let Some((algorithm, digest)) = value.split_once(':') else {
            return Err(StableIdError::new(
                &value,
                "content hashes must use algorithm:lowercase-hex",
            ));
        };
        if algorithm.is_empty()
            || !algorithm
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            || digest.is_empty()
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(StableIdError::new(
                &value,
                "content hashes must use algorithm:lowercase-hex",
            ));
        }
        Ok(Self(value))
    }

    /// Returns the algorithm-qualified hash text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for ContentHash {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_ids_are_strict_and_deserialization_validates() {
        assert_eq!(
            ComponentSchemaId::new("yuyib.transform-3d")
                .expect("valid id")
                .as_str(),
            "yuyib.transform-3d"
        );
        for invalid in [
            "transform",
            "Yuyib.transform",
            "yuyib..transform",
            "yuyib._transform",
            "yuyib.transform_3d",
            "yuyib.transform-",
        ] {
            assert!(ComponentSchemaId::new(invalid).is_err(), "{invalid}");
        }
        assert!(serde_json::from_str::<SystemId>(r#""Yuyib.system""#).is_err());
    }

    #[test]
    fn schema_versions_reject_zero_in_code_and_json() {
        assert!(SchemaVersion::new(0).is_err());
        assert!(serde_json::from_str::<SchemaVersion>("0").is_err());
        assert_eq!(SchemaVersion::new(3).expect("valid version").get(), 3);
    }

    #[test]
    fn asset_and_entity_guid_are_persistent_but_strongly_typed() {
        let asset = AssetGuid::new();
        let json = serde_json::to_string(&asset).expect("serialize guid");
        assert_eq!(
            serde_json::from_str::<AssetGuid>(&json).expect("deserialize guid"),
            asset
        );
        let entity = EntityGuid::from_uuid(asset.as_uuid());
        assert_eq!(entity.as_uuid(), asset.as_uuid());
    }

    #[test]
    fn content_hash_is_not_an_asset_identity() {
        assert!(ContentHash::new("sha256:12ab90ef").is_ok());
        assert!(ContentHash::new("12ab90ef").is_err());
        assert!(ContentHash::new("sha256:12AB").is_err());
        assert!(serde_json::from_str::<ContentHash>(r#""sha256:12AB""#).is_err());
    }
}
