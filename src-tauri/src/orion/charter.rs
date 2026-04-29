//! Local mirror of the SAO-signed charter document.
//!
//! The charter is the OrionII analogue of what older code called the "soul"
//! — a Markdown document that scopes what this entity is commissioned to do.
//! Every `Envelope` on the bus carries `soul_ref = blake3(charter_bytes)`,
//! so a content-addressed change to the charter is visible from the event
//! log alone (the version-violence guardrail).
//!
//! The local copy lives next to `config.json` at
//! `%APPDATA%\OrionII\charter.md` — operators can open it the same way they
//! open the bundle config. A placeholder is written before commissioning so
//! `current_soul_ref` always has stable bytes to hash. The signed copy +
//! certificate live in SAO; OrionII never has the private key material that
//! signed it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// Text written to `charter.md` before the operator runs commissioning.
/// Hashing this is harmless — every fresh OrionII install has the same
/// placeholder hash, and `soul_ref` flips to a real, mentor-and-SAO-signed
/// hash the moment commissioning's `charter.update` envelope is applied.
pub const PLACEHOLDER_CHARTER: &str = "# Uncommissioned\n\nThis entity has not yet been commissioned. Run commissioning to define a charter and bind it to a SAO-issued birth certificate.\n";

/// In-memory mirror of `charter.md`. The bytes are exactly what got hashed
/// to produce `soul_ref`, so reading the file once at boot and again on
/// `charter.update` is enough — there is no third source of truth.
pub struct Charter {
    path: PathBuf,
    bytes: Vec<u8>,
}

impl Charter {
    /// Open the charter at the standard local path, writing the placeholder
    /// if no file exists yet. Errors here fall back to an in-memory
    /// placeholder so a read-only filesystem can't brick boot — `soul_ref`
    /// still hashes deterministically.
    pub fn load_or_init_placeholder() -> Self {
        let path = default_charter_path();
        Self::load_or_init_placeholder_at(path)
    }

    pub fn load_or_init_placeholder_at(path: PathBuf) -> Self {
        match fs::read(&path) {
            Ok(bytes) => Self { path, bytes },
            Err(_) => {
                let bytes = PLACEHOLDER_CHARTER.as_bytes().to_vec();
                if let Some(parent) = path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                let _ = fs::write(&path, &bytes);
                Self { path, bytes }
            }
        }
    }

    /// Replace the on-disk charter and the in-memory mirror in one step.
    /// Called from the `governance` subscriber on `charter.update`.
    pub fn replace(&mut self, text: &str) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.path, text.as_bytes())?;
        self.bytes = text.as_bytes().to_vec();
        Ok(())
    }

    #[allow(dead_code)] // used by upcoming commissioning client slice
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[allow(dead_code)] // used by upcoming commissioning UI slice for display
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `hex(blake3(charter_bytes))`. Stable across processes — the hash of
    /// identical bytes is byte-for-byte identical regardless of platform,
    /// which is the whole point of using it as `soul_ref`.
    pub fn hash(&self) -> String {
        blake3::hash(&self.bytes).to_hex().to_string()
    }
}

/// Shared, hot-swappable charter cell. Owned by `OrionCore`, cloned to
/// every participant that publishes `Envelope`s. The `RwLock` is a
/// `std::sync::RwLock` rather than tokio's because the read happens
/// synchronously on the publish path and held only for the duration of one
/// `hash()` call.
pub type SharedCharter = Arc<RwLock<Charter>>;

pub fn shared(charter: Charter) -> SharedCharter {
    Arc::new(RwLock::new(charter))
}

fn default_charter_path() -> PathBuf {
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return PathBuf::from(appdata).join("OrionII").join("charter.md");
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.join("charter.md");
        }
    }
    std::env::temp_dir().join("OrionII").join("charter.md")
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn temp_charter_path() -> PathBuf {
        std::env::temp_dir()
            .join(format!("orionii-charter-test-{}", Uuid::new_v4()))
            .join("charter.md")
    }

    #[test]
    fn load_or_init_writes_placeholder_when_missing() {
        let path = temp_charter_path();
        let charter = Charter::load_or_init_placeholder_at(path.clone());

        assert_eq!(charter.bytes(), PLACEHOLDER_CHARTER.as_bytes());
        assert!(path.exists());

        let from_disk = fs::read_to_string(&path).unwrap();
        assert_eq!(from_disk, PLACEHOLDER_CHARTER);

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn round_trip_hash_is_blake3_of_bytes() {
        let path = temp_charter_path();
        let mut charter = Charter::load_or_init_placeholder_at(path.clone());

        let text = "# Real charter\n\nDo the work.\n";
        charter.replace(text).unwrap();

        let expected = blake3::hash(text.as_bytes()).to_hex().to_string();
        assert_eq!(charter.hash(), expected);
        assert_eq!(charter.bytes(), text.as_bytes());

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn placeholder_hash_is_stable_across_loads() {
        let path = temp_charter_path();
        let first = Charter::load_or_init_placeholder_at(path.clone()).hash();
        let second = Charter::load_or_init_placeholder_at(path.clone()).hash();
        assert_eq!(first, second);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
