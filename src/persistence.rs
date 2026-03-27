/// Trait for types that can be loaded from and saved to JSON files.
///
/// Provides default implementations for the common pattern:
/// - Load: read file, return Default on NotFound, deserialize
/// - Save: serialize to pretty JSON, create parent dirs, write
use serde::{Serialize, de::DeserializeOwned};
use std::io;
use std::path::Path;

pub trait JsonPersistable: Sized + Default + Serialize + DeserializeOwned {
    fn load_json(path: &Path) -> io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => {
                serde_json::from_str(&s).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    fn save_json(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, json)
    }
}
