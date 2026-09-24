//! Preliminary server comparison under one attested topology.
//! This selects a load source, not proof of unshaped line capacity.

use sha2::{Digest, Sha256};

pub(crate) const REPEATS: usize = 3;
pub(crate) const MIN_STABLE_SERVERS: usize = 2;
pub(crate) const SERVERS_PER_BATCH: usize = 3;
pub(crate) const LEGACY_MAX_SERVERS: usize = 3;
pub(crate) const MAX_SERVERS: usize = 6;
pub(crate) const MAX_COMPARISONS: usize = MAX_SERVERS * REPEATS + 1;
pub(crate) const MAX_REPORT_BYTES: usize = 64 * 1024;
const MIN_STABILITY_PERCENT: u128 = 80;
const MIN_RELATIVE_THROUGHPUT_PERCENT: u128 = 80;
pub(crate) const LEGACY_POLICY: &str = "median-three-v1";
pub(crate) const LEGACY_DISTINCT_POLICY: &str = "median-three-distinct-provider-host-v2";
pub(crate) const LEGACY_BOUNDED_POLICY: &str = "median-three-distinct-provider-host-bounded-v3";
pub(crate) const SOURCE_POLICY: &str = "median-three-distinct-provider-host-fallback-v4";

pub(crate) fn normalized_provider(value: &str) -> Option<String> {
    if value.is_empty()
        || value.len() > 1024
        || value.chars().count() > 256
        || value.chars().any(char::is_control)
    {
        return None;
    }
    let normalized = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    (!normalized.is_empty()).then_some(normalized)
}

/// Hostname identity only: ports, schemes and paths cannot create independent
/// sources. No DNS lookup or physical-path independence is implied.
pub(crate) fn endpoint_host(url: &str) -> Result<String, &'static str> {
    if url.len() > 2048
        || !url.is_ascii()
        || url
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return Err("speedtest-server-endpoint-invalid");
    }
    let (scheme, rest) = url
        .split_once("://")
        .ok_or("speedtest-server-endpoint-invalid")?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Err("speedtest-server-endpoint-invalid");
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        return Err("speedtest-server-endpoint-invalid");
    }
    let host = if let Some((host, port)) = authority.rsplit_once(':') {
        if port.is_empty()
            || !port.bytes().all(|byte| byte.is_ascii_digit())
            || port.parse::<u16>().ok().is_none_or(|port| port == 0)
        {
            return Err("speedtest-server-endpoint-invalid");
        }
        host
    } else {
        authority
    };
    let host = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    if host.is_empty()
        || host.len() > 253
        || host.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        return Err("speedtest-server-endpoint-invalid");
    }
    if host
        .bytes()
        .all(|byte| byte.is_ascii_digit() || byte == b'.')
        && host.parse::<std::net::Ipv4Addr>().is_err()
    {
        return Err("speedtest-server-endpoint-invalid");
    }
    Ok(host)
}

pub(crate) fn endpoint_identity(url: &str) -> Result<(String, String), String> {
    let host = endpoint_host(url)?;
    let (scheme, rest) = url
        .split_once("://")
        .ok_or("speedtest-server-endpoint-invalid")?;
    let scheme = scheme.to_ascii_lowercase();
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let port = authority
        .rsplit_once(':')
        .map(|(_, port)| port.parse::<u16>())
        .transpose()
        .map_err(|_| "speedtest-server-endpoint-invalid")?
        .unwrap_or(if scheme == "https" { 443 } else { 80 });
    let path_query = rest[end..].split('#').next().unwrap_or_default();
    let slash = if path_query.starts_with('/') { "" } else { "/" };
    let canonical = format!("{scheme}://{host}:{port}{slash}{path_query}");
    let digest = format!("{:x}", Sha256::digest(canonical.as_bytes()));
    Ok((host, digest))
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Observation {
    pub server_id: u64,
    pub download_kbps: u64,
    pub upload_kbps: u64,
}

/// Reference from this run's fully unshaped qualification, not a physical cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct QualifiedCapacity {
    pub download_kbps: u64,
    pub upload_kbps: u64,
}

impl QualifiedCapacity {
    pub(crate) fn from_selected(
        observations: &[Observation],
        selected: u64,
    ) -> Result<Self, &'static str> {
        select(observations, Some(selected))?;
        let mut download = [0; REPEATS];
        let mut upload = [0; REPEATS];
        let mut count = 0;
        for observation in observations
            .iter()
            .filter(|value| value.server_id == selected)
        {
            if count >= REPEATS {
                return Err("server-comparison-repeat-bound");
            }
            download[count] = observation.download_kbps;
            upload[count] = observation.upload_kbps;
            count += 1;
        }
        if count != REPEATS {
            return Err("server-comparison-selected-reference-missing");
        }
        download.sort_unstable();
        upload.sort_unstable();
        Ok(Self {
            download_kbps: download[REPEATS / 2],
            upload_kbps: upload[REPEATS / 2],
        })
    }

    pub(crate) fn attest_raw_rate(
        &self,
        direction: super::protocol::SpeedtestDirection,
        achieved_kbps: u64,
    ) -> Result<(), &'static str> {
        let reference = match direction {
            super::protocol::SpeedtestDirection::Download => self.download_kbps,
            super::protocol::SpeedtestDirection::Upload => self.upload_kbps,
            super::protocol::SpeedtestDirection::Both => {
                return Err("server-comparison-directional-reference-required")
            }
        };
        if reference == 0 || achieved_kbps == 0 {
            return Err("server-comparison-selected-reference-missing");
        }
        // Reuse the comparison's 80% consistency threshold. A low raw sample
        // is uncertain source/link drift, never permission for a new low cap.
        if u128::from(achieved_kbps) * 100 < u128::from(reference) * MIN_RELATIVE_THROUGHPUT_PERCENT
        {
            return Err("server-or-link-capacity-changed");
        }
        Ok(())
    }
}

/// One logical comparison, including all its charged route-loss retries.
/// Offset/count index the job's contiguous qualification debit records.
#[derive(Clone, Debug)]
pub(crate) struct Comparison {
    pub index: usize,
    pub candidate_id: Option<u64>,
    pub server_id: Option<u64>,
    pub server_name: String,
    pub server_sponsor: String,
    pub endpoint_host: Option<String>,
    pub endpoint_sha256: Option<String>,
    pub display_metadata_truncated: bool,
    pub started_boot_ms: u64,
    pub elapsed_ms: u64,
    pub debit_offset: u32,
    pub debit_count: u32,
    pub download_kbps: Option<u64>,
    pub upload_kbps: Option<u64>,
    pub valid: bool,
    pub code: String,
}

pub(crate) fn comparisons_json(comparisons: &[Comparison]) -> Result<serde_json::Value, String> {
    if comparisons.len() > MAX_COMPARISONS {
        return Err("server report comparison bound".into());
    }
    let mut next_debit = 0u32;
    let mut previous_index = None;
    let mut rows = Vec::with_capacity(comparisons.len());
    for value in comparisons {
        if value.index > MAX_SERVERS * REPEATS
            || (previous_index.is_none() && value.index != 0)
            || previous_index.is_some_and(|previous| value.index <= previous)
            || value.debit_offset != next_debit
            || value.code.len() > 128
        {
            return Err("server report comparison order or debit binding".into());
        }
        if value.index == 0
            && (value.valid || value.candidate_id.is_some() || value.server_id.is_some())
        {
            return Err("server report discovery row is not a server observation".into());
        }
        if value.valid
            && (value.server_id.is_none_or(|id| id == 0)
                || value.candidate_id != value.server_id
                || value.debit_count == 0
                || value.download_kbps.is_none_or(|rate| rate == 0)
                || value.upload_kbps.is_none_or(|rate| rate == 0))
        {
            return Err("server report valid observation lacks exact source or traffic".into());
        }
        next_debit = next_debit
            .checked_add(value.debit_count)
            .ok_or("server report debit overflow")?;
        previous_index = Some(value.index);
        let mut row = serde_json::json!({
            "index": value.index, "candidate_id": value.candidate_id, "server_id": value.server_id,
            "server_name": value.server_name.chars().take(256).collect::<String>(),
            "server_sponsor": value.server_sponsor.chars().take(256).collect::<String>(),
            "display_metadata_truncated": value.display_metadata_truncated || value.server_name.chars().count() > 256 || value.server_sponsor.chars().count() > 256,
            "started_boot_ms": value.started_boot_ms, "elapsed_ms": value.elapsed_ms,
            "debit_offset": value.debit_offset, "debit_count": value.debit_count,
            "download_kbps": value.download_kbps, "upload_kbps": value.upload_kbps,
            "valid_observation": value.valid, "reason": value.code,
        });
        if value.endpoint_host.is_some() != value.endpoint_sha256.is_some() {
            return Err("server report endpoint identity is incomplete".into());
        }
        if let Some(host) = value.endpoint_host.as_deref() {
            if endpoint_host(&format!("http://{host}/"))? != host {
                return Err("server report endpoint is not canonical".into());
            }
            row["endpoint_host"] = serde_json::json!(host);
            let digest = value
                .endpoint_sha256
                .as_deref()
                .ok_or("server report endpoint identity is incomplete")?;
            if digest.len() != 64
                || !digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err("server report endpoint digest invalid".into());
            }
            row["endpoint_sha256"] = serde_json::json!(digest);
        }
        rows.push(row);
    }
    Ok(serde_json::Value::Array(rows))
}

pub(crate) fn parse_comparisons(value: &serde_json::Value) -> Result<Vec<Comparison>, String> {
    let rows = value
        .as_array()
        .filter(|rows| rows.len() <= MAX_COMPARISONS)
        .ok_or("server report rows bound")?;
    let mut comparisons = Vec::with_capacity(rows.len());
    for row in rows {
        let number = |key: &str| {
            row.get(key)
                .and_then(serde_json::Value::as_u64)
                .ok_or("server report numeric field")
        };
        let optional = |key: &str| -> Result<Option<u64>, String> {
            match row.get(key) {
                Some(serde_json::Value::Null) => Ok(None),
                Some(value) => value
                    .as_u64()
                    .map(Some)
                    .ok_or_else(|| "server report optional number".into()),
                None => Err("server report missing field".into()),
            }
        };
        let text = |key: &str| {
            row.get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or("server report text field")
        };
        comparisons.push(Comparison {
            index: number("index")?
                .try_into()
                .map_err(|_| "server report index overflow")?,
            candidate_id: optional("candidate_id")?,
            server_id: optional("server_id")?,
            server_name: text("server_name")?,
            server_sponsor: text("server_sponsor")?,
            endpoint_host: row
                .get("endpoint_host")
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .ok_or("server report endpoint field")
                })
                .transpose()?,
            endpoint_sha256: row
                .get("endpoint_sha256")
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .ok_or("server report endpoint digest field")
                })
                .transpose()?,
            display_metadata_truncated: row
                .get("display_metadata_truncated")
                .and_then(serde_json::Value::as_bool)
                .ok_or("server report truncation field")?,
            started_boot_ms: number("started_boot_ms")?,
            elapsed_ms: number("elapsed_ms")?,
            debit_offset: number("debit_offset")?
                .try_into()
                .map_err(|_| "server report offset overflow")?,
            debit_count: number("debit_count")?
                .try_into()
                .map_err(|_| "server report count overflow")?,
            download_kbps: optional("download_kbps")?,
            upload_kbps: optional("upload_kbps")?,
            valid: row
                .get("valid_observation")
                .and_then(serde_json::Value::as_bool)
                .ok_or("server report validity field")?,
            code: text("reason")?,
        });
    }
    if comparisons_json(&comparisons)? != *value {
        return Err("server report rows are not canonical".into());
    }
    Ok(comparisons)
}

#[derive(Clone, Copy, Default)]
struct Candidate {
    id: u64,
    count: usize,
    dl: [u64; REPEATS],
    ul: [u64; REPEATS],
}

pub(crate) fn select(
    observations: &[Observation],
    requested: Option<u64>,
) -> Result<u64, &'static str> {
    if observations.len() > MAX_SERVERS * REPEATS {
        return Err("server-comparison-observation-bound");
    }
    let mut candidates = [Candidate::default(); MAX_SERVERS];
    for observation in observations {
        if observation.server_id == 0
            || observation.download_kbps == 0
            || observation.upload_kbps == 0
        {
            return Err("server-comparison-invalid-observation");
        }
        let index = candidates
            .iter()
            .position(|candidate| candidate.id == observation.server_id)
            .or_else(|| candidates.iter().position(|candidate| candidate.id == 0))
            .ok_or("server-comparison-candidate-bound")?;
        let candidate = &mut candidates[index];
        if candidate.count == REPEATS {
            return Err("server-comparison-repeat-bound");
        }
        candidate.id = observation.server_id;
        candidate.dl[candidate.count] = observation.download_kbps;
        candidate.ul[candidate.count] = observation.upload_kbps;
        candidate.count += 1;
    }
    let mut stable = 0;
    let mut best_dl = 0;
    let mut best_ul = 0;
    for candidate in &mut candidates {
        if candidate.count != REPEATS {
            candidate.id = 0;
            continue;
        }
        candidate.dl.sort_unstable();
        candidate.ul.sort_unstable();
        let consistent = [candidate.dl, candidate.ul].iter().all(|rates| {
            u128::from(rates[0]) * 100 >= u128::from(rates[REPEATS - 1]) * MIN_STABILITY_PERCENT
        });
        if !consistent {
            candidate.id = 0;
            continue;
        }
        stable += 1;
        best_dl = best_dl.max(candidate.dl[1]);
        best_ul = best_ul.max(candidate.ul[1]);
    }
    if stable < MIN_STABLE_SERVERS {
        return Err("server-comparison-insufficient-stable-candidates");
    }
    let mut selected: Option<(u128, u64)> = None;
    for candidate in candidates.iter().filter(|candidate| candidate.id != 0) {
        if u128::from(candidate.dl[1]) * 100 < u128::from(best_dl) * MIN_RELATIVE_THROUGHPUT_PERCENT
            || u128::from(candidate.ul[1]) * 100
                < u128::from(best_ul) * MIN_RELATIVE_THROUGHPUT_PERCENT
        {
            continue;
        }
        if requested.is_some_and(|id| id != candidate.id) {
            continue;
        }
        let score = (u128::from(candidate.dl[1]) * 1_000_000 / u128::from(best_dl))
            .min(u128::from(candidate.ul[1]) * 1_000_000 / u128::from(best_ul));
        if selected.is_none_or(|(previous, id)| {
            score > previous || (score == previous && candidate.id < id)
        }) {
            selected = Some((score, candidate.id));
        }
    }
    selected.map(|(_, id)| id).ok_or(if requested.is_some() {
        "server-comparison-requested-server-not-competitive"
    } else {
        "server-comparison-no-comparable-bidirectional-server"
    })
}

/// Require a stable independent reported provider/hostname alongside the
/// selected source. A different ID, port or spelling is not corroboration.
pub(crate) fn select_independent(
    comparisons: &[Comparison],
    requested: Option<u64>,
) -> Result<u64, &'static str> {
    if comparisons.len() > MAX_COMPARISONS {
        return Err("server-comparison-observation-bound");
    }
    let observations: Vec<_> = comparisons
        .iter()
        .filter(|row| row.valid)
        .map(|row| Observation {
            server_id: row.server_id.unwrap_or(0),
            download_kbps: row.download_kbps.unwrap_or(0),
            upload_kbps: row.upload_kbps.unwrap_or(0),
        })
        .collect();
    let selected = select(&observations, requested)?;
    let source = |id: u64| -> Result<Option<(&str, String)>, &'static str> {
        let mut identity: Option<(&str, String)> = None;
        let mut pinned_endpoint = None;
        let mut dl = [0; REPEATS];
        let mut ul = [0; REPEATS];
        let mut count = 0;
        for row in comparisons
            .iter()
            .filter(|row| row.valid && row.server_id == Some(id))
        {
            let host = row
                .endpoint_host
                .as_deref()
                .ok_or("server-comparison-endpoint-missing")?;
            let pin = row
                .endpoint_sha256
                .as_deref()
                .ok_or("server-comparison-endpoint-missing")?;
            if pinned_endpoint.is_some_and(|previous| previous != pin) {
                return Err("server-comparison-source-identity-changed");
            }
            pinned_endpoint = Some(pin);
            let provider = normalized_provider(&row.server_sponsor)
                .ok_or("server-comparison-provider-missing")?;
            if identity
                .as_ref()
                .is_some_and(|old| old.0 != host || old.1 != provider)
            {
                return Err("server-comparison-source-identity-changed");
            }
            if count >= REPEATS {
                return Err("server-comparison-repeat-bound");
            }
            identity = Some((host, provider));
            dl[count] = row.download_kbps.unwrap_or(0);
            ul[count] = row.upload_kbps.unwrap_or(0);
            count += 1;
        }
        if count != REPEATS {
            return Ok(None);
        }
        dl.sort_unstable();
        ul.sort_unstable();
        if [dl, ul].iter().any(|rates| {
            rates[0] == 0
                || u128::from(rates[0]) * 100
                    < u128::from(rates[REPEATS - 1]) * MIN_STABILITY_PERCENT
        }) {
            return Ok(None);
        }
        Ok(identity)
    };
    let selected_source =
        source(selected)?.ok_or("server-comparison-selected-reference-missing")?;
    for row in comparisons
        .iter()
        .filter(|row| row.valid && row.server_id != Some(selected))
    {
        if let Some(other) = source(row.server_id.unwrap_or(0))? {
            if other.0 != selected_source.0 && other.1 != selected_source.1 {
                return Ok(selected);
            }
        }
    }
    Err("server-comparison-insufficient-independent-sources")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn observations(rates: &[(u64, u64, u64)]) -> Vec<Observation> {
        (0..REPEATS)
            .flat_map(|_| {
                rates
                    .iter()
                    .map(|&(server_id, download_kbps, upload_kbps)| Observation {
                        server_id,
                        download_kbps,
                        upload_kbps,
                    })
            })
            .collect()
    }
    #[test]
    fn t1_reported_sources_require_distinct_stable_hosts_and_providers() {
        let rows = |host: &str, provider: &str| -> Vec<Comparison> {
            (0..REPEATS)
                .flat_map(|round| {
                    [(1, "one.example.invalid", "First ISP"), (2, host, provider)]
                        .into_iter()
                        .map(move |(id, host, provider)| Comparison {
                            index: round * 2 + id as usize,
                            candidate_id: Some(id),
                            server_id: Some(id),
                            server_name: format!("server-{id}"),
                            server_sponsor: provider.into(),
                            endpoint_host: Some(host.into()),
                            endpoint_sha256: Some(format!("{id:064x}")),
                            display_metadata_truncated: false,
                            started_boot_ms: 1,
                            elapsed_ms: 1000,
                            debit_offset: (round * 2 + id as usize - 1) as u32,
                            debit_count: 1,
                            download_kbps: Some(900_000),
                            upload_kbps: Some(500_000),
                            valid: true,
                            code: "valid-observation".into(),
                        })
                })
                .collect()
        };
        let valid = rows("two.example.invalid", "Second ISP");
        assert_eq!(select_independent(&valid, None), Ok(1));
        assert_eq!(select_independent(&valid, Some(2)), Ok(2));
        // A rejected first batch does not exhaust the bounded discovery pool.
        // Every failed attempt still occupies a report/debit slot.
        let mut fallback = Vec::new();
        let mut discovery = valid[0].clone();
        discovery.index = 0;
        discovery.candidate_id = None;
        discovery.server_id = None;
        discovery.valid = false;
        discovery.download_kbps = None;
        discovery.upload_kbps = None;
        discovery.code = "server-list-complete".into();
        fallback.push(discovery);
        for id in 1..=SERVERS_PER_BATCH {
            let mut failed = valid[0].clone();
            failed.index = id;
            failed.candidate_id = Some(id as u64);
            failed.server_id = None;
            failed.valid = false;
            failed.download_kbps = None;
            failed.upload_kbps = None;
            failed.code = "speedtest-backend-failed".into();
            failed.debit_offset = id as u32;
            fallback.push(failed);
        }
        assert!(select_independent(&fallback, None).is_err());
        for (index, row) in valid.iter().enumerate() {
            let mut backup = row.clone();
            backup.index = SERVERS_PER_BATCH * REPEATS + index + 1;
            backup.server_id = row.server_id.map(|id| id + 3);
            backup.candidate_id = backup.server_id;
            backup.debit_offset = fallback.len() as u32;
            fallback.push(backup);
        }
        assert_eq!(select_independent(&fallback, None), Ok(4));
        assert!(select_independent(&fallback, Some(1)).is_err());
        let encoded = comparisons_json(&fallback).unwrap();
        let decoded = parse_comparisons(&encoded).unwrap();
        assert_eq!(select_independent(&decoded, None), Ok(4));
        assert_eq!(decoded.last().unwrap().debit_offset, 9);
        assert!(decoded.len() <= MAX_COMPARISONS);
        assert_eq!(
            select_independent(&rows("one.example.invalid", "Second ISP"), None),
            Err("server-comparison-insufficient-independent-sources")
        );
        assert_eq!(
            select_independent(&rows("two.example.invalid", "  FIRST   isp "), None),
            Err("server-comparison-insufficient-independent-sources")
        );
        let mut changed = valid.clone();
        changed[2].endpoint_host = Some("moved.example.invalid".into());
        assert_eq!(
            select_independent(&changed, None),
            Err("server-comparison-source-identity-changed")
        );
        let mut missing = valid;
        let mut changed_pin = missing.clone();
        changed_pin[2].endpoint_sha256 = Some("f".repeat(64));
        assert_eq!(
            select_independent(&changed_pin, None),
            Err("server-comparison-source-identity-changed")
        );
        missing[1].endpoint_host = None;
        assert_eq!(
            select_independent(&missing, None),
            Err("server-comparison-endpoint-missing")
        );
    }

    #[test]
    fn t1_endpoint_aliases_cannot_become_independent_sources() {
        assert_eq!(
            endpoint_identity("HTTP://Example.com").unwrap(),
            endpoint_identity("http://example.com.:80/#ignored").unwrap()
        );
        let first = endpoint_identity("http://example.com:8080/upload.php").unwrap();
        let other_port = endpoint_identity("http://example.com:8081/upload.php").unwrap();
        let other_path = endpoint_identity("http://example.com:8080/other.php").unwrap();
        assert_eq!(first.0, other_port.0);
        assert_ne!(first.1, other_port.1);
        assert_ne!(first.1, other_path.1);
        for url in [
            "http://Example.COM/speedtest/upload.php",
            "HTTPS://example.com.:443/other?token=private",
            "http://example.com:8080/path",
        ] {
            assert_eq!(endpoint_host(url), Ok("example.com".into()));
        }
        assert_eq!(
            endpoint_host("http://192.0.2.1:8080/path"),
            Ok("192.0.2.1".into())
        );
        for invalid in [
            "http://user:secret@example.com/",
            "ftp://example.com/",
            "http://example.com:0/",
            "http://example.com:65536/",
            "http://127.1/",
            "http://example..com/",
            "http://example.com\n/",
            "http://[::1]/",
        ] {
            assert_eq!(
                endpoint_host(invalid),
                Err("speedtest-server-endpoint-invalid")
            );
        }
    }

    #[test]
    fn t1_selected_reference_rejects_raw_collapse_but_accepts_consistent_or_faster_rates() {
        use super::super::protocol::SpeedtestDirection::{Both, Download, Upload};
        let input = observations(&[(1, 900_000, 500_000), (2, 910_000, 510_000)]);
        let reference = QualifiedCapacity::from_selected(&input, 1).unwrap();
        assert_eq!(reference.download_kbps, 900_000);
        assert_eq!(reference.upload_kbps, 500_000);
        assert_eq!(
            reference.attest_raw_rate(Download, 167_500),
            Err("server-or-link-capacity-changed")
        );
        assert!(reference.attest_raw_rate(Download, 719_999).is_err());
        assert!(reference.attest_raw_rate(Download, 720_000).is_ok());
        assert!(reference.attest_raw_rate(Upload, 400_000).is_ok());
        assert!(reference.attest_raw_rate(Download, 1_000_000).is_ok());
        assert!(reference.attest_raw_rate(Upload, 0).is_err());
        assert!(reference.attest_raw_rate(Both, 900_000).is_err());
        assert!(QualifiedCapacity::from_selected(&input, 99).is_err());
        assert!(QualifiedCapacity::from_selected(&input[..2], 1).is_err());
        let slow = observations(&[(1, 16_750, 8_000), (2, 16_800, 8_100)]);
        assert!(QualifiedCapacity::from_selected(&slow, 1)
            .unwrap()
            .attest_raw_rate(Download, 16_750)
            .is_ok());
        let maximum = QualifiedCapacity {
            download_kbps: u64::MAX,
            upload_kbps: u64::MAX,
        };
        assert!(maximum.attest_raw_rate(Download, u64::MAX).is_ok());
    }

    #[test]
    fn t1_slow_first_server_cannot_win_over_repeated_fast_alternative() {
        let input = observations(&[(1, 167_500, 500_000), (2, 900_000, 500_000)]);
        assert_eq!(select(&input, None), Ok(2));
        assert_eq!(
            select(&input, Some(1)),
            Err("server-comparison-requested-server-not-competitive")
        );
        assert_eq!(select(&input, Some(2)), Ok(2));
    }
    #[test]
    fn t1_one_server_or_single_fast_outlier_is_not_independent_repeated_evidence() {
        assert!(select(&observations(&[(1, 900_000, 500_000)]), None).is_err());
        let mut input = observations(&[(1, 167_500, 500_000), (2, 167_500, 500_000)]);
        input[1].download_kbps = 900_000;
        assert_eq!(
            select(&input, None),
            Err("server-comparison-insufficient-stable-candidates")
        );
        assert!(select(&input[..2], None).is_err());
    }
    #[test]
    fn t1_consistent_slow_links_are_not_labeled_bad_and_directional_maxima_are_not_fused() {
        assert_eq!(
            select(
                &observations(&[(1, 100_000, 50_000), (2, 100_000, 50_000)]),
                None
            ),
            Ok(1)
        );
        assert_eq!(
            select(
                &observations(&[(1, 900_000, 50_000), (2, 100_000, 500_000)]),
                None
            ),
            Err("server-comparison-no-comparable-bidirectional-server")
        );
    }
}
