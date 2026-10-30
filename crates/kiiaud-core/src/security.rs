// SPDX-License-Identifier: MIT OR Apache-2.0

//! Security policy for local filesystem paths (`R-023`, Phase 6).
//!
//! Prevents path traversal (`..`), enforces configurable `allowed_roots` whitelist,
//! resolves symlinks safely, and rejects device/proc/sys pseudo-files.

use crate::error::{KiiaudError, Result};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct PathPolicy {
    allowed_roots: Vec<PathBuf>,
}

impl PathPolicy {
    /// Create a policy with the given allowed root directories.
    /// If `allowed_roots` is empty, any regular file without `..` traversal outside normal directories is permitted,
    /// except `/proc`, `/sys`, `/dev`, and `/etc`.
    pub fn new(allowed_roots: Vec<PathBuf>) -> Self {
        let normalized_roots = allowed_roots
            .into_iter()
            .map(|r| std::fs::canonicalize(&r).unwrap_or(r))
            .collect();
        Self {
            allowed_roots: normalized_roots,
        }
    }

    /// Validate and canonicalize a local filesystem path before opening it for playback.
    pub fn validate_local_path(&self, path: &Path) -> Result<PathBuf> {
        if path.as_os_str().is_empty() {
            return Err(KiiaudError::SecurityViolation("Empty file path".to_owned()));
        }

        // Reject explicit parent directory traversal (`..`) in raw input path
        if path.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(KiiaudError::SecurityViolation(format!(
                "Path traversal ('..') is forbidden: {}",
                path.display()
            )));
        }

        // If path is relative and we have a primary allowed root, join onto the first allowed root
        let resolved_candidate = if path.is_relative() {
            if let Some(first_root) = self.allowed_roots.first() {
                first_root.join(path)
            } else {
                return Err(KiiaudError::SecurityViolation(format!(
                    "Relative path not allowed when no allowed_roots configured: {}",
                    path.display()
                )));
            }
        } else {
            path.to_path_buf()
        };

        // Canonicalize to resolve symlinks and verify existence
        let canonical = std::fs::canonicalize(&resolved_candidate).map_err(|e| {
            KiiaudError::Source(format!(
                "Cannot access '{}': {}",
                resolved_candidate.display(),
                e
            ))
        })?;

        // Always forbid system pseudo-filesystems and sensitive directories
        for forbidden in ["/proc", "/sys", "/dev", "/etc", "/boot", "/root"] {
            if canonical.starts_with(forbidden) && !self.is_explicitly_allowed(&canonical) {
                return Err(KiiaudError::SecurityViolation(format!(
                    "Access to system directory '{}' is forbidden",
                    canonical.display()
                )));
            }
        }

        // Enforce allowed_roots whitelist if configured
        if !self.allowed_roots.is_empty() && !self.is_explicitly_allowed(&canonical) {
            return Err(KiiaudError::SecurityViolation(format!(
                "Path '{}' is outside configured allowed_roots",
                canonical.display()
            )));
        }

        // Ensure it is a regular file (not a directory, socket, FIFO, or block/char device)
        let metadata = std::fs::metadata(&canonical).map_err(|e| {
            KiiaudError::Source(format!("Cannot stat '{}': {}", canonical.display(), e))
        })?;

        if !metadata.is_file() {
            return Err(KiiaudError::SecurityViolation(format!(
                "Path '{}' is not a regular audio file",
                canonical.display()
            )));
        }

        Ok(canonical)
    }

    fn is_explicitly_allowed(&self, canonical_path: &Path) -> bool {
        self.allowed_roots
            .iter()
            .any(|root| canonical_path.starts_with(root))
    }
}
