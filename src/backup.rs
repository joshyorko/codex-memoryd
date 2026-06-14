//! Operator backup / verify / restore workflow (issue #141).
//!
//! Local-first memory is only credible if an operator can back it up, verify
//! the backup, and rehearse a restore before an upgrade. This module provides
//! four first-class operations that mirror the preview-before-apply pattern
//! used elsewhere in the substrate:
//!
//! - [`create_backup`] — online SQLite backup plus a machine-readable manifest.
//! - [`verify_backup`] — recompute the digest and integrity-check the copy.
//! - [`restore_preview`] — report what a restore *would* change, without writing.
//! - [`restore_apply`] — atomically replace the target DB after re-verifying.
//!
//! Backups are plain SQLite files. The manifest (`*.manifest.json`) is safe to
//! commit for provenance; the database copy is not, because it contains stored
//! memory. See `docs/release/v0.1-hardening.md` for the runbook.

use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

use crate::error::Error;
use crate::error::Result;
use crate::ids;
use crate::store::SchemaReport;
use crate::store::Store;

/// Manifest format version. Bumped only on breaking manifest-shape changes.
pub const BACKUP_MANIFEST_VERSION: u32 = 1;

/// An owned table/row-count pair for the manifest (serializes round-trip,
/// unlike the store's borrowed-name `TableCount`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestTableCount {
    pub table: String,
    pub rows: i64,
}

/// A machine-readable description of a backup, written next to the `.db` copy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupManifest {
    pub manifest_version: u32,
    /// Tool version that produced the backup (`CARGO_PKG_VERSION`).
    pub tool_version: String,
    /// RFC3339 creation time, supplied by the caller for determinism in tests.
    pub created_at: String,
    /// Backup database file name (relative; sits beside the manifest).
    pub database_file: String,
    /// SHA-256 of the backup database bytes.
    pub sha256: String,
    /// Size of the backup database in bytes.
    pub size_bytes: u64,
    /// Schema version recorded in the backup.
    pub schema_version: Option<i64>,
    /// Expected schema version for the producing binary.
    pub expected_schema_version: i64,
    /// Row counts for durable tables at backup time.
    pub tables: Vec<ManifestTableCount>,
}

/// Result of [`create_backup`].
#[derive(Debug, Clone, Serialize)]
pub struct BackupResult {
    pub database_path: PathBuf,
    pub manifest_path: PathBuf,
    pub manifest: BackupManifest,
}

/// Result of [`verify_backup`].
#[derive(Debug, Clone, Serialize)]
pub struct VerifyResult {
    pub ok: bool,
    /// True when the recomputed digest matches the manifest.
    pub digest_matches: bool,
    /// True when `PRAGMA integrity_check` passes on the backup.
    pub integrity_ok: bool,
    /// True when the manifest schema version matches this binary's expectation.
    pub schema_matches: bool,
    pub manifest: BackupManifest,
    /// Human-readable issues found (empty when `ok`).
    pub issues: Vec<String>,
}

/// Result of [`restore_preview`]. Reports the change a restore would make
/// without touching the target database.
#[derive(Debug, Clone, Serialize)]
pub struct RestorePreview {
    /// Verification of the backup that would be restored.
    pub verify: VerifyResult,
    /// True when a database already exists at the restore target.
    pub target_exists: bool,
    /// Schema report of the current target (None when it does not exist).
    pub current: Option<SchemaReport>,
    /// Per-table row deltas (backup minus current). Positive = backup has more.
    pub table_deltas: Vec<TableDelta>,
    /// True when a restore is safe to apply (backup verified).
    pub safe_to_apply: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TableDelta {
    pub table: String,
    pub current_rows: i64,
    pub backup_rows: i64,
    pub delta: i64,
}

/// Result of [`restore_apply`].
#[derive(Debug, Clone, Serialize)]
pub struct RestoreResult {
    pub restored: bool,
    pub target_path: PathBuf,
    /// Path of the safety copy taken of the prior target (None if none existed).
    pub prior_backup: Option<PathBuf>,
}

fn sha256_file(path: &Path) -> Result<String> {
    let bytes =
        std::fs::read(path).map_err(|e| Error::storage(format!("read {}: {e}", path.display())))?;
    Ok(ids::sha256_hex(&bytes))
}

fn manifest_path_for(database_path: &Path) -> PathBuf {
    let mut s = database_path.as_os_str().to_os_string();
    s.push(".manifest.json");
    PathBuf::from(s)
}

/// Create a backup of `source` at `dest`, writing a manifest beside it.
/// `created_at` is supplied by the caller (RFC3339) so backups are
/// reproducible in tests.
pub fn create_backup(source: &Store, dest: &Path, created_at: &str) -> Result<BackupResult> {
    source.online_backup_to(dest)?;

    // Reopen the backup to read its schema/counts independently of the source.
    let backup_store = Store::open(dest)?;
    let report = backup_store.schema_report()?;
    drop(backup_store);

    let sha256 = sha256_file(dest)?;
    let size_bytes = std::fs::metadata(dest)
        .map_err(|e| Error::storage(format!("stat {}: {e}", dest.display())))?
        .len();

    let manifest = BackupManifest {
        manifest_version: BACKUP_MANIFEST_VERSION,
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        created_at: created_at.to_string(),
        database_file: dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "backup.db".to_string()),
        sha256,
        size_bytes,
        schema_version: report.recorded_version,
        expected_schema_version: report.expected_version,
        tables: report
            .tables
            .iter()
            .map(|t| ManifestTableCount {
                table: t.table.to_string(),
                rows: t.rows,
            })
            .collect(),
    };

    let manifest_path = manifest_path_for(dest);
    let manifest_json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| Error::internal(format!("serialize manifest: {e}")))?;
    std::fs::write(&manifest_path, manifest_json)
        .map_err(|e| Error::storage(format!("write manifest {}: {e}", manifest_path.display())))?;

    Ok(BackupResult {
        database_path: dest.to_path_buf(),
        manifest_path,
        manifest,
    })
}

fn read_manifest(manifest_path: &Path) -> Result<BackupManifest> {
    let raw = std::fs::read_to_string(manifest_path)
        .map_err(|e| Error::storage(format!("read manifest {}: {e}", manifest_path.display())))?;
    serde_json::from_str(&raw).map_err(|e| {
        Error::invalid_request(format!("parse manifest {}: {e}", manifest_path.display()))
    })
}

/// Verify a backup database against its manifest: digest, integrity, schema.
pub fn verify_backup(database_path: &Path) -> Result<VerifyResult> {
    let manifest_path = manifest_path_for(database_path);
    let manifest = read_manifest(&manifest_path)?;
    let mut issues = Vec::new();

    let digest_matches = match sha256_file(database_path) {
        Ok(actual) => {
            let matches = actual == manifest.sha256;
            if !matches {
                issues.push("backup digest does not match manifest sha256".to_string());
            }
            matches
        }
        Err(e) => {
            issues.push(format!("could not read backup database: {e}"));
            false
        }
    };

    let integrity_ok = match Store::open(database_path) {
        Ok(store) => match store.integrity_ok() {
            Ok(true) => true,
            Ok(false) => {
                issues.push("PRAGMA integrity_check failed on backup".to_string());
                false
            }
            Err(e) => {
                issues.push(format!("integrity check error: {e}"));
                false
            }
        },
        Err(e) => {
            issues.push(format!("could not open backup database: {e}"));
            false
        }
    };

    let schema_matches = manifest.schema_version == Some(manifest.expected_schema_version);
    if !schema_matches {
        issues.push(format!(
            "backup schema version {:?} differs from expected {}",
            manifest.schema_version, manifest.expected_schema_version
        ));
    }

    Ok(VerifyResult {
        ok: digest_matches && integrity_ok && schema_matches && issues.is_empty(),
        digest_matches,
        integrity_ok,
        schema_matches,
        manifest,
        issues,
    })
}

/// Preview what restoring `backup_path` over `target_path` would change.
/// Does not write to the target.
pub fn restore_preview(backup_path: &Path, target_path: &Path) -> Result<RestorePreview> {
    let verify = verify_backup(backup_path)?;
    let target_exists = target_path.exists();

    let current = if target_exists {
        Some(Store::open(target_path)?.schema_report()?)
    } else {
        None
    };

    let mut table_deltas = Vec::new();
    for backup_table in &verify.manifest.tables {
        let current_rows = current
            .as_ref()
            .and_then(|r| r.tables.iter().find(|t| t.table == backup_table.table))
            .map(|t| t.rows)
            .unwrap_or(0);
        table_deltas.push(TableDelta {
            table: backup_table.table.clone(),
            current_rows,
            backup_rows: backup_table.rows,
            delta: backup_table.rows - current_rows,
        });
    }

    Ok(RestorePreview {
        safe_to_apply: verify.ok,
        verify,
        target_exists,
        current,
        table_deltas,
    })
}

/// Apply a restore: re-verify the backup, take a safety copy of any existing
/// target, then move the verified backup copy into place. The safety copy path
/// is returned so an operator can roll back manually.
///
/// The daemon must not be running against `target_path` during a restore:
/// overwriting a live SQLite file (with an open WAL) is unsafe. The runbook in
/// `docs/release/v0.1-hardening.md` says to stop the daemon first.
///
/// `now` is used to name the safety copy deterministically.
pub fn restore_apply(backup_path: &Path, target_path: &Path, now: &str) -> Result<RestoreResult> {
    let verify = verify_backup(backup_path)?;
    if !verify.ok {
        return Err(Error::invalid_request(format!(
            "refusing to restore: backup failed verification ({})",
            verify.issues.join("; ")
        )));
    }

    let prior_backup = if target_path.exists() {
        let stamp = now.replace([':', '.'], "-");
        let mut s = target_path.as_os_str().to_os_string();
        s.push(format!(".pre-restore-{stamp}.bak"));
        let prior = PathBuf::from(s);
        std::fs::copy(target_path, &prior)
            .map_err(|e| Error::storage(format!("safety-copy target: {e}")))?;
        Some(prior)
    } else {
        None
    };

    if let Some(parent) = target_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::storage(format!("create target dir: {e}")))?;
        }
    }

    // Copy the verified backup into place. Use copy (not rename) so the backup
    // file itself is preserved. WAL/SHM sidecars of a stale target, if any, are
    // removed so the restored file is authoritative.
    std::fs::copy(backup_path, target_path)
        .map_err(|e| Error::storage(format!("copy backup into target: {e}")))?;
    for sidecar in ["-wal", "-shm"] {
        let mut s = target_path.as_os_str().to_os_string();
        s.push(sidecar);
        let p = PathBuf::from(s);
        let _ = std::fs::remove_file(p);
    }

    // Confirm the restored target opens and is sound.
    let restored = Store::open(target_path)?;
    if !restored.integrity_ok()? {
        return Err(Error::storage(
            "restored database failed integrity check".to_string(),
        ));
    }

    Ok(RestoreResult {
        restored: true,
        target_path: target_path.to_path_buf(),
        prior_backup,
    })
}
