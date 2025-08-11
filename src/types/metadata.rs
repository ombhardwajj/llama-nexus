use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct Metadata(HashMap<String, String>);

// Custom deserializer to validate constraints during JSON deserialization
impl<'de> Deserialize<'de> for Metadata {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let map = HashMap::<String, String>::deserialize(deserializer)?;
        Self::from_map(map).map_err(serde::de::Error::custom)
    }
}

impl Metadata {
    /// Create a new empty Metadata
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    /// Create Metadata from a HashMap, validating constraints
    pub fn from_map(map: HashMap<String, String>) -> Result<Self, MetadataError> {
        let metadata = Self(map);
        metadata.validate()?;
        Ok(metadata)
    }

    /// Insert a key-value pair, validating constraints
    pub fn insert(&mut self, key: String, value: String) -> Result<(), MetadataError> {
        // Validate constraints
        if self.0.len() >= 16 && !self.0.contains_key(&key) {
            return Err(MetadataError::TooManyKeys);
        }
        if key.len() > 64 {
            return Err(MetadataError::KeyTooLong(key.len()));
        }
        if value.len() > 512 {
            return Err(MetadataError::ValueTooLong(value.len()));
        }

        self.0.insert(key, value);
        Ok(())
    }

    /// Get a value by key
    pub fn get(&self, key: &str) -> Option<&String> {
        self.0.get(key)
    }

    /// Remove a key-value pair
    pub fn remove(&mut self, key: &str) -> Option<String> {
        self.0.remove(key)
    }

    /// Get the number of key-value pairs
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Check if metadata is empty
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Get an iterator over key-value pairs
    pub fn iter(&self) -> std::collections::hash_map::Iter<String, String> {
        self.0.iter()
    }

    /// Convert to HashMap
    pub fn into_inner(self) -> HashMap<String, String> {
        self.0
    }

    /// Get reference to inner HashMap
    pub fn as_map(&self) -> &HashMap<String, String> {
        &self.0
    }

    /// Validate all constraints
    fn validate(&self) -> Result<(), MetadataError> {
        if self.0.len() > 16 {
            return Err(MetadataError::TooManyKeys);
        }

        for (key, value) in &self.0 {
            if key.len() > 64 {
                return Err(MetadataError::KeyTooLong(key.len()));
            }
            if value.len() > 512 {
                return Err(MetadataError::ValueTooLong(value.len()));
            }
        }

        Ok(())
    }
}

// Use this instead of From<HashMap> to enforce validation
impl TryFrom<HashMap<String, String>> for Metadata {
    type Error = MetadataError;
    
    fn try_from(map: HashMap<String, String>) -> Result<Self, Self::Error> {
        Self::from_map(map)
    }
}

// Removed the non-validating From<HashMap> trait for safety
// Always use TryFrom or from_map() to ensure validation

// JSON serialization/deserialization methods for sqlx compatibility
impl Metadata {
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(&self.0)
    }
    
    pub fn from_json(json_str: &str) -> Result<Self, serde_json::Error> {
        let data: HashMap<String, String> = serde_json::from_str(json_str)?;
        // Note: Database data is assumed to be already validated
        // If you want strict validation, use from_map() instead
        Ok(Metadata(data))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataError {
    TooManyKeys,
    KeyTooLong(usize),
    ValueTooLong(usize),
}

impl std::fmt::Display for MetadataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MetadataError::TooManyKeys => {
                write!(f, "Metadata cannot have more than 16 key-value pairs")
            }
            MetadataError::KeyTooLong(len) => {
                write!(f, "Metadata key too long: {} characters (max 64)", len)
            }
            MetadataError::ValueTooLong(len) => {
                write!(f, "Metadata value too long: {} characters (max 512)", len)
            }
        }
    }
}

impl std::error::Error for MetadataError {}