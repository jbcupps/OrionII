//! Iggy Personal Access Token (PAT) lifecycle for the bundled-sidecar path.
//!
//! Phase 2b stores the PAT in a per-user file at
//! `{config_dir}/OrionII/iggy_pat` (Windows: `%APPDATA%\OrionII\iggy_pat`).
//! On Unix the file is `chmod 600`; on Windows it inherits the per-user
//! `%APPDATA%` ACL. Phase 2.1 will swap the file backend for an OS keychain
//! integration (e.g. `keyring` crate, deferred because of native-deps build
//! complexity).
//!
//! Bootstrap flow:
//! 1. First run, no `iggy_pat` file: connect to the sidecar with the
//!    iggy-server bootstrap admin credentials (`iggy`/`iggy` by default —
//!    documented in iggy's docs as the dev-bootstrap pair), provision a
//!    long-lived PAT scoped to the entity stream, write it to disk, change
//!    the admin password to a randomly-generated value, and discard both
//!    the admin password and the bootstrap login from memory.
//! 2. Subsequent runs: read the PAT from `iggy_pat`, connect with it.
//! 3. Rotation: provision a new PAT via the existing PAT, delete the old
//!    one server-side, write the new one to `iggy_pat`. Exposed as the
//!    `rotate_iggy_token` Tauri command.

use std::path::{Path, PathBuf};

use thiserror::Error;

/// Personal Access Token + the endpoint it's bound to. Persisted as JSON.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IggyCredentials {
    pub endpoint: String,
    pub pat: String,
}

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("could not determine config dir for the PAT store")]
    NoConfigDir,
    #[error("PAT store I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("PAT store decode: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("Iggy provisioning failed: {0}")]
    Provision(String),
}

/// Resolve the on-disk path for the PAT file.
pub fn pat_store_path() -> Result<PathBuf, AuthError> {
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let mut p = PathBuf::from(appdata);
        p.push("OrionII");
        return Ok(p.join("iggy_pat"));
    }
    if let Ok(home) = std::env::var("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".config");
        p.push("OrionII");
        return Ok(p.join("iggy_pat"));
    }
    Err(AuthError::NoConfigDir)
}

/// Try to load existing credentials from the PAT store. Returns `None`
/// if the file doesn't exist (first-run signal). Other I/O errors bubble.
pub fn load(path: &Path) -> Result<Option<IggyCredentials>, AuthError> {
    if !path.exists() {
        return Ok(None);
    }
    let contents = std::fs::read_to_string(path)?;
    let creds: IggyCredentials = serde_json::from_str(&contents)?;
    Ok(Some(creds))
}

/// Persist credentials with restricted permissions. The directory is
/// created if missing.
pub fn save(path: &Path, creds: &IggyCredentials) -> Result<(), AuthError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(creds)?;
    std::fs::write(path, json)?;
    set_restricted_permissions(path)?;
    Ok(())
}

/// On Unix: chmod 600. On Windows: rely on the per-user `%APPDATA%` ACL
/// (Windows file ACLs through std are messy; the user-only directory
/// already provides isolation in practice).
#[cfg(unix)]
fn set_restricted_permissions(path: &Path) -> Result<(), AuthError> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_restricted_permissions(_path: &Path) -> Result<(), AuthError> {
    // Windows: %APPDATA% is per-user; the parent ACL provides isolation.
    // Phase 2.1 keychain integration will replace this entirely.
    Ok(())
}

/// Phase 2b stub for the first-run provisioning flow. Wired into
/// `iggy_supervisor`'s post-start callback. Today returns a deterministic
/// dev token so the bundled-Iggy path can compile and run end-to-end with
/// the iggy-server's default admin login; the proper PAT-mint-and-rotate
/// dance is a TODO marked below.
///
/// TODO(phase-2b-pat-mint): replace this with an actual call to
/// `client.create_personal_access_token(...)` once the IggyBus connect
/// path is wired and we can reuse its admin client. Until then, the
/// bundled path uses iggy-server's default credentials directly — that's
/// fine for single-user single-machine dev but must change before any
/// multi-tenant or cloud deploy.
pub async fn provision_first_run(endpoint: &str) -> Result<IggyCredentials, AuthError> {
    Ok(IggyCredentials {
        endpoint: endpoint.to_string(),
        // Iggy's documented bootstrap pair, used as a stand-in PAT until
        // the real PAT-mint flow lands. See TODO above.
        pat: "iggy:iggy".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_and_load_round_trips_credentials() {
        let dir = std::env::temp_dir().join(format!("orionii-pat-{}", uuid::Uuid::new_v4()));
        let path = dir.join("iggy_pat");

        let original = IggyCredentials {
            endpoint: "tcp://127.0.0.1:8090".to_string(),
            pat: "test-token-abc".to_string(),
        };
        save(&path, &original).unwrap();

        let loaded = load(&path).unwrap().expect("creds present");
        assert_eq!(loaded.endpoint, original.endpoint);
        assert_eq!(loaded.pat, original.pat);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn load_returns_none_when_file_missing() {
        let dir = std::env::temp_dir().join(format!("orionii-pat-{}", uuid::Uuid::new_v4()));
        let path = dir.join("iggy_pat");

        let result = load(&path).unwrap();
        assert!(result.is_none());
    }
}
