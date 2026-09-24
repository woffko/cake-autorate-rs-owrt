//! Immutable historical-confidence decision and bounded same-boot archive.

use super::autotune_apply::native_apply_sha256_hex;
use super::autotune_apply_runtime::{
    ensure_private_directory, read_private_recovery_bounded, replace_private_file,
    require_private_directory, sync_directory, write_new_private_file,
};
use serde_json::{json, Value};
use std::path::Path;

const MAX_BYTES: usize = 4096;
const MAX_ROUTES: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RawReference {
    pub job_id: String,
    pub review_sha256: String,
    pub created_unix_ms: u64,
    pub download_kbps: u64,
    pub upload_kbps: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HistoryDecision {
    pub boot_id: String,
    pub route_key: String,
    pub request_sha256: String,
    pub worker_run_id: String,
    pub current: RawReference,
    pub reference: Option<RawReference>,
}

fn valid_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl RawReference {
    fn validate(&self) -> Result<(), String> {
        if !valid_hex(&self.job_id, 32)
            || !valid_hex(&self.review_sha256, 64)
            || self.created_unix_ms == 0
            || self.download_kbps == 0
            || self.upload_kbps == 0
            || self.download_kbps > 100_000_000
            || self.upload_kbps > 100_000_000
        {
            return Err("history reference is invalid".into());
        }
        Ok(())
    }

    fn value(&self) -> Value {
        json!({"job_id":self.job_id, "review_sha256":self.review_sha256,
            "created_unix_ms":self.created_unix_ms,
            "download_kbps":self.download_kbps, "upload_kbps":self.upload_kbps})
    }

    fn parse(value: &Value) -> Result<Self, String> {
        Ok(Self {
            job_id: text_field(value, "job_id")?,
            review_sha256: text_field(value, "review_sha256")?,
            created_unix_ms: number_field(value, "created_unix_ms")?,
            download_kbps: number_field(value, "download_kbps")?,
            upload_kbps: number_field(value, "upload_kbps")?,
        })
    }
}

fn text_field(value: &Value, name: &str) -> Result<String, String> {
    value[name]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("history {name} is invalid"))
}

fn number_field(value: &Value, name: &str) -> Result<u64, String> {
    value[name]
        .as_u64()
        .ok_or_else(|| format!("history {name} is invalid"))
}

impl HistoryDecision {
    pub fn blocked(&self) -> bool {
        self.reference.as_ref().is_some_and(|reference| {
            u128::from(self.current.download_kbps) * 2 < u128::from(reference.download_kbps)
                || u128::from(self.current.upload_kbps) * 2 < u128::from(reference.upload_kbps)
        })
    }

    pub fn validate(&self) -> Result<(), String> {
        if !valid_hex(&self.boot_id, 32)
            || !valid_hex(&self.route_key, 64)
            || !valid_hex(&self.request_sha256, 64)
            || !valid_hex(&self.worker_run_id, 32)
        {
            return Err("history decision binding is invalid".into());
        }
        self.current.validate()?;
        if let Some(reference) = &self.reference {
            reference.validate()?;
            if reference.job_id == self.current.job_id
                || reference.created_unix_ms >= self.current.created_unix_ms
            {
                return Err("history reference is not an earlier distinct job".into());
            }
        }
        Ok(())
    }

    /// Only pass a canonically verified prior decision with the exact route key.
    /// A blocked predecessor carries its trusted reference, never its low sample.
    pub fn inherit_reference(&mut self, previous: &Self) -> Result<(), String> {
        self.validate()?;
        previous.validate()?;
        if self.reference.is_some()
            || self.boot_id != previous.boot_id
            || self.route_key != previous.route_key
            || previous.current.created_unix_ms >= self.current.created_unix_ms
            || previous.current.job_id == self.current.job_id
        {
            return Err("history predecessor does not match this decision".into());
        }
        let reference = if previous.blocked() {
            previous
                .reference
                .as_ref()
                .ok_or("blocked history has no reference")?
        } else {
            &previous.current
        };
        self.reference = Some(reference.clone());
        self.validate()
    }

    pub fn encode(&self) -> Result<String, String> {
        self.validate()?;
        let payload = json!({"schema_version":1, "boot_id":self.boot_id,
            "route_key":self.route_key, "request_sha256":self.request_sha256,
            "worker_run_id":self.worker_run_id, "current":self.current.value(),
            "reference":self.reference.as_ref().map(RawReference::value), "blocked":self.blocked()});
        // Integrity checksum, not a signature against a privileged writer.
        let digest = native_apply_sha256_hex(payload.to_string().as_bytes());
        let encoded = format!("{}\n", json!({"payload":payload,"sha256":digest}));
        if encoded.len() > MAX_BYTES {
            return Err("history decision exceeds bound".into());
        }
        Ok(encoded)
    }

    pub fn decode(input: &str) -> Result<Self, String> {
        if input.len() > MAX_BYTES {
            return Err("history decision exceeds bound".into());
        }
        let value: Value = serde_json::from_str(input).map_err(|_| "history JSON is invalid")?;
        let payload = &value["payload"];
        let decision = Self {
            boot_id: text_field(payload, "boot_id")?,
            route_key: text_field(payload, "route_key")?,
            request_sha256: text_field(payload, "request_sha256")?,
            worker_run_id: text_field(payload, "worker_run_id")?,
            current: RawReference::parse(&payload["current"])?,
            reference: if payload["reference"].is_null() {
                None
            } else {
                Some(RawReference::parse(&payload["reference"])?)
            },
        };
        // Exact encoding rejects duplicate/unknown fields, changed checksum,
        // mismatched derived block flag and noncanonical numeric spellings.
        if decision.encode()? != input {
            return Err("history decision is noncanonical or corrupt".into());
        }
        Ok(decision)
    }

    pub fn read(path: &Path) -> Result<Self, String> {
        require_private_directory(path.parent().ok_or("history decision has no directory")?)?;
        let bytes = read_private_recovery_bounded(path, MAX_BYTES, "history decision")?;
        Self::decode(std::str::from_utf8(&bytes).map_err(|_| "history decision is not UTF-8")?)
    }

    /// Keep one compact decision per route outside prunable job directories.
    /// This cache remains same-boot only and must not replace per-job binding.
    pub fn archive(&self, directory: &Path) -> Result<(), String> {
        self.validate()?;
        ensure_private_directory(directory)?;
        let path = directory.join(&self.route_key);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {
                let previous = Self::read(&path)?;
                if previous.boot_id != self.boot_id || previous.route_key != self.route_key {
                    return Err("history archive binding changed".into());
                }
                if previous.current.job_id == self.current.job_id {
                    if previous != *self {
                        return Err("history archive decision changed".into());
                    }
                    return sync_directory(directory);
                }
                if previous.current.created_unix_ms >= self.current.created_unix_ms {
                    return Err("history archive refuses an out-of-order decision".into());
                }
                // A new seed must not erase a prior hold.
                let mut projected = self.clone();
                projected.reference = None;
                projected.inherit_reference(&previous)?;
                if projected != *self {
                    return Err("history archive trust chain changed".into());
                }
                replace_private_file(
                    &path,
                    &directory.join(".history-next"),
                    self.encode()?.as_bytes(),
                )?;
                sync_directory(directory)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let entries = std::fs::read_dir(directory)
                    .map_err(|error| format!("unable to list history archive: {error}"))?;
                for (index, entry) in entries.enumerate() {
                    entry.map_err(|error| format!("unable to inspect history archive: {error}"))?;
                    if index >= MAX_ROUTES - 1 {
                        return Err("history archive route bound reached".into());
                    }
                }
                self.publish(&path)
            }
            Err(error) => Err(format!("unable to inspect history archive: {error}")),
        }
    }

    pub fn publish(&self, path: &Path) -> Result<(), String> {
        let encoded = self.encode()?;
        let parent = path.parent().ok_or("history decision has no directory")?;
        require_private_directory(parent)?;
        match std::fs::symlink_metadata(path) {
            Ok(_) => {
                if Self::read(path)? != *self {
                    return Err("history decision is immutable".into());
                }
                sync_directory(parent)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                write_new_private_file(path, encoded.as_bytes())?;
                sync_directory(parent)
            }
            Err(error) => Err(format!("unable to inspect history decision: {error}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn decision(id: char, time: u64, dl: u64) -> HistoryDecision {
        HistoryDecision {
            boot_id: "a".repeat(32),
            route_key: "b".repeat(64),
            request_sha256: id.to_string().repeat(64),
            worker_run_id: id.to_string().repeat(32),
            current: RawReference {
                job_id: id.to_string().repeat(32),
                review_sha256: id.to_string().repeat(64),
                created_unix_ms: time,
                download_kbps: dl,
                upload_kbps: 100000,
            },
            reference: None,
        }
    }

    #[test]
    fn t3_blocked_retests_cannot_replace_the_trusted_reference() {
        let first = decision('1', 1, 900000);
        let mut low = decision('2', 2, 170000);
        low.inherit_reference(&first).unwrap();
        assert!(low.blocked());
        let restored = HistoryDecision::decode(&low.encode().unwrap()).unwrap();
        let mut repeated = decision('3', 3, 170000);
        repeated.inherit_reference(&restored).unwrap();
        assert!(repeated.blocked());
        assert_eq!(repeated.reference, Some(first.current));
        let mut recovered = decision('4', 4, 800000);
        recovered.inherit_reference(&repeated).unwrap();
        assert!(!recovered.blocked());
        let mut later = decision('5', 5, 790000);
        later.inherit_reference(&recovered).unwrap();
        assert_eq!(later.reference, Some(recovered.current));
        for variant in 0..3 {
            let mut foreign = decision('6', 6, 900000);
            match variant {
                0 => foreign.boot_id = "0".repeat(32),
                1 => foreign.route_key = "0".repeat(64),
                _ => foreign.current.created_unix_ms = 1,
            }
            assert!(foreign.inherit_reference(&later).is_err());
            assert!(foreign.reference.is_none());
        }
    }

    #[test]
    fn t3_archive_preserves_hold_after_source_job_files_are_retired() {
        let root =
            std::env::temp_dir().join(format!("cake-history-archive-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let job = root.join("job");
        ensure_private_directory(&job).unwrap();
        let archive = root.join("history");
        let first = decision('1', 1, 900000);
        first.publish(&job.join("decision")).unwrap();
        first.archive(&archive).unwrap();
        let mut low = decision('2', 2, 170000);
        low.inherit_reference(&first).unwrap();
        low.archive(&archive).unwrap();
        low.archive(&archive).unwrap();
        std::fs::remove_dir_all(&job).unwrap();
        let restored = HistoryDecision::read(&archive.join(&low.route_key)).unwrap();
        assert!(restored.blocked());
        let mut retest = decision('3', 3, 170000);
        assert!(
            retest.archive(&archive).is_err(),
            "new seed cannot erase a prior hold"
        );
        retest.inherit_reference(&restored).unwrap();
        retest.archive(&archive).unwrap();
        assert_eq!(retest.reference, Some(first.current.clone()));
        assert!(
            first.archive(&archive).is_err(),
            "old settlement cannot roll back history"
        );
        assert_eq!(
            HistoryDecision::read(&archive.join(&retest.route_key)).unwrap(),
            retest
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn t3_history_decision_is_bounded_canonical_and_immutable() {
        let root =
            std::env::temp_dir().join(format!("cake-history-decision-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("decision");
        let mut value = decision('1', 1, 900000);
        value.publish(&path).unwrap();
        value.publish(&path).unwrap();
        assert_eq!(HistoryDecision::read(&path).unwrap(), value);
        let encoded = value.encode().unwrap();
        for corrupt in [
            encoded.replace("900000", "900001"),
            encoded.replace("\"blocked\":false", "\"blocked\":true"),
            encoded.replace("\"payload\":", "\"payload\":{},\"payload\":"),
            format!(" {encoded}"),
            "x".repeat(MAX_BYTES + 1),
        ] {
            assert!(HistoryDecision::decode(&corrupt).is_err());
        }
        value.current.download_kbps = 170000;
        assert!(value.publish(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), encoded);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(HistoryDecision::read(&path).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = root.join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(HistoryDecision::read(&link).is_err());
        assert!(value.publish(&link).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
