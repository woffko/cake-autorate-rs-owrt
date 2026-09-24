//! Shared pure rendering of owned IPv4 route marking, accounting and egress rules.
//! The caller supplies bounded process execution and validates socket credentials.
//! Error identifiers and byte output preserve existing worker/recovery contracts.

pub(crate) const ACCOUNTING_RX_COUNTER: &str = "rx";
pub(crate) const ACCOUNTING_TX_COUNTER: &str = "tx";
pub(crate) const ACCOUNTING_FAULT_COUNTER: &str = "flow_fault";
pub(crate) const FLOW_ACCOUNTING_OWNER_SUFFIX: &str = ":flow-v1";
pub(crate) const MAX_ACCOUNTING_FLOWS: u32 = 4096;
const MAX_NFT_SNAPSHOT_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy)]
pub(crate) enum NftSocketOwner {
    BackendUid(u32),
    ProbeRootGid(u32),
}

/// Submit one atomic exclusive-create batch. No named temporary file is needed.
/// Callers retain their credential lease and attest ownership before using it.
pub(crate) fn install_owned_route_pin_with(
    table: &str,
    owner: &str,
    socket_owner: NftSocketOwner,
    route_mark: Option<(u32, u32)>,
    device: &str,
    execute: impl FnOnce(&[&str], &[u8]) -> Result<bool, String>,
) -> Result<(), String> {
    let batch = nft_owned_route_pin_batch(table, owner, socket_owner, route_mark);
    let batch = nft_egress_guard_batch(&batch, table, socket_owner, device)?;
    if !execute(&["-j", "-f", "-"], batch.as_bytes())? {
        return Err("speedtest-route-pin-install-failed".into());
    }
    Ok(())
}

pub(crate) fn nft_owned_route_pin_batch(
    table: &str,
    owner: &str,
    socket_owner: NftSocketOwner,
    route_mark: Option<(u32, u32)>,
) -> String {
    use serde_json::json;
    let (uid, probe_gid) = match socket_owner {
        NftSocketOwner::BackendUid(uid) => (uid, None),
        NftSocketOwner::ProbeRootGid(gid) => (0, Some(gid)),
    };
    let counter_owner = format!("{owner}{FLOW_ACCOUNTING_OWNER_SUFFIX}");
    let key = json!({"concat": [
        {"ct":{"key":"ip saddr","dir":"original"}},
        {"ct":{"key":"ip daddr","dir":"original"}},
        {"ct":{"key":"protocol"}},
        {"ct":{"key":"proto-src","dir":"original"}},
        {"ct":{"key":"proto-dst","dir":"original"}}
    ]});
    let ipv4 = json!({"match":{"op":"==","left":{"meta":{"key":"nfproto"}},"right":"ipv4"}});
    let default_zone = json!({"match":{"op":"==","left":{"ct":{"key":"zone"}},"right":0}});
    let original =
        json!({"match":{"op":"==","left":{"ct":{"key":"direction"}},"right":"original"}});
    let owned = json!({"match":{"op":"==","left":{"meta":{"key":"skuid"}},"right":uid}});
    let member = json!({"match":{"op":"==","left":key,"right":"@owned_flows"}});
    let mut commands = Vec::with_capacity(14);
    // The preceding absence check is not a reservation. Fail atomically if
    // another owner claims this name before the batch reaches the kernel.
    commands.push(json!({"create":{"table":{"family":"inet","name":table,"comment":owner}}}));
    // Entries live until exact table cleanup. A short inactivity timeout can
    // silently lose delayed replies. The finite size instead fails closed.
    commands.push(
        json!({"add":{"set":{"family":"inet","table":table,"name":"owned_flows",
        "type":["ipv4_addr","ipv4_addr","inet_proto","inet_service","inet_service"],
        "size":MAX_ACCOUNTING_FLOWS}}}),
    );
    for name in [
        ACCOUNTING_RX_COUNTER,
        ACCOUNTING_TX_COUNTER,
        ACCOUNTING_FAULT_COUNTER,
    ] {
        commands.push(json!({"add":{"counter":{"family":"inet","table":table,"name":name,"comment":counter_owner}}}));
    }
    for (hook, kind) in [("output", "route"), ("input", "filter")] {
        commands.push(
            json!({"add":{"chain":{"family":"inet","table":table,"name":hook,
            "type":kind,"hook":hook,"prio":-148,"policy":"accept"}}}),
        );
    }
    let mut transmit = vec![
        ipv4.clone(),
        default_zone.clone(),
        original.clone(),
        member.clone(),
        json!({"counter":ACCOUNTING_TX_COUNTER}),
    ];
    if let Some((clear_mask, fwmark)) = route_mark {
        transmit.push(json!({"mangle":{"key":{"meta":{"key":"mark"}},
            "value":{"|":[{"&":[{"meta":{"key":"mark"}},clear_mask]},fwmark]}}}));
    }
    transmit.push(json!({"return":null}));
    let mut register = vec![
        ipv4.clone(),
        default_zone.clone(),
        original.clone(),
        owned.clone(),
    ];
    let mut fault = vec![owned.clone()];
    if let Some(gid) = probe_gid {
        let group = json!({"match":{"op":"==","left":{"meta":{"key":"skgid"}},"right":gid}});
        register.push(group.clone());
        fault.push(group);
    }
    register.push(json!({"set":{"op":"update","set":"@owned_flows","elem":key}}));
    fault.extend([
        json!({"counter":ACCOUNTING_FAULT_COUNTER}),
        json!({"drop":null}),
    ]);
    let mut rules = vec![
        ("output", register),
        // Clear a reused tuple before charging it. Membership still attributes
        // late kernel packets after their original socket file was released.
        (
            "output",
            vec![
                ipv4.clone(),
                default_zone.clone(),
                original.clone(),
                json!({"match":{"op":"!=","left":{"meta":{"key":"skuid"}},"right":uid}}),
                json!({"set":{"op":"delete","set":"@owned_flows","elem":key}}),
            ],
        ),
    ];
    if let Some(gid) = probe_gid {
        // Complement of root AND this GID: non-root is handled above; root
        // with another GID must also clear a reused probe tuple before debit.
        rules.push((
            "output",
            vec![
                ipv4.clone(),
                default_zone.clone(),
                original.clone(),
                owned.clone(),
                json!({"match":{"op":"!=","left":{"meta":{"key":"skgid"}},"right":gid}}),
                json!({"set":{"op":"delete","set":"@owned_flows","elem":key}}),
            ],
        ));
    }
    // libc's system resolver can use ::1 even when the measured WAN is IPv4.
    // Admit only loopback DNS tuples, never unpinned IPv6 WAN traffic. Keep
    // socket ownership, finite registration, stale-tuple clearing and reply
    // accounting identical to the IPv4 path; do not exempt DNS from the ledger.
    let dns6_key = json!({"concat": [
        {"ct":{"key":"protocol"}},
        {"ct":{"key":"proto-src","dir":"original"}},
        {"ct":{"key":"proto-dst","dir":"original"}}
    ]});
    commands.push(json!({"add":{"set":{"family":"inet","table":table,
        "name":"owned_dns6","type":["inet_proto","inet_service","inet_service"],
        "size":MAX_ACCOUNTING_FLOWS}}}));
    let dns6_scope = vec![
        json!({"match":{"op":"==","left":{"meta":{"key":"nfproto"}},"right":"ipv6"}}),
        default_zone.clone(),
        json!({"match":{"op":"==","left":{"ct":{"key":"ip6 saddr","dir":"original"}},"right":"::1"}}),
        json!({"match":{"op":"==","left":{"ct":{"key":"ip6 daddr","dir":"original"}},"right":"::1"}}),
        json!({"match":{"op":"in","left":{"ct":{"key":"protocol"}},"right":{"set":["tcp","udp"]}}}),
        json!({"match":{"op":"==","left":{"ct":{"key":"proto-dst","dir":"original"}},"right":53}}),
    ];
    let mut dns6_output = dns6_scope.clone();
    dns6_output.push(json!({"match":{"op":"==","left":{"meta":{"key":"oifname"}},"right":"lo"}}));
    dns6_output.push(original.clone());
    let mut register6 = dns6_output.clone();
    register6.push(owned.clone());
    if let Some(gid) = probe_gid {
        register6.push(json!({"match":{"op":"==","left":{"meta":{"key":"skgid"}},"right":gid}}));
    }
    register6.push(json!({"set":{"op":"update","set":"@owned_dns6","elem":dns6_key}}));
    rules.push(("output", register6));
    let mut clear6 = dns6_output.clone();
    clear6.push(json!({"match":{"op":"!=","left":{"meta":{"key":"skuid"}},"right":uid}}));
    clear6.push(json!({"set":{"op":"delete","set":"@owned_dns6","elem":dns6_key}}));
    rules.push(("output", clear6));
    if let Some(gid) = probe_gid {
        let mut clear_group6 = dns6_output.clone();
        clear_group6.push(owned);
        clear_group6.push(json!({"match":{"op":"!=","left":{"meta":{"key":"skgid"}},"right":gid}}));
        clear_group6.push(json!({"set":{"op":"delete","set":"@owned_dns6","elem":dns6_key}}));
        rules.push(("output", clear_group6));
    }
    let dns6_member = json!({"match":{"op":"==","left":dns6_key,"right":"@owned_dns6"}});
    dns6_output.extend([
        dns6_member.clone(),
        json!({"counter":ACCOUNTING_TX_COUNTER}),
        json!({"return":null}),
    ]);
    rules.push(("output", dns6_output));
    let mut receive6 = dns6_scope;
    receive6.extend([
        json!({"match":{"op":"==","left":{"meta":{"key":"iifname"}},"right":"lo"}}),
        json!({"match":{"op":"==","left":{"ct":{"key":"direction"}},"right":"reply"}}),
        dns6_member,
        json!({"counter":ACCOUNTING_RX_COUNTER}),
    ]);
    rules.push(("input", receive6));
    rules.extend([
        ("output", transmit),
        // No CT-dependent condition here: NOTRACK, unsupported family/zone or
        // full-set failure must not let this backend send unaccounted traffic.
        ("output", fault),
        (
            "input",
            vec![
                ipv4,
                default_zone,
                json!({"match":{"op":"==","left":{"ct":{"key":"direction"}},"right":"reply"}}),
                member,
                json!({"counter":ACCOUNTING_RX_COUNTER}),
            ],
        ),
    ]);
    for (chain, expressions) in rules {
        commands.push(json!({"add":{"rule":{"family":"inet","table":table,"chain":chain,"expr":expressions}}}));
    }
    format!("{}\n", json!({"nftables":commands}))
}

pub(crate) fn nft_egress_guard_batch(
    batch: &str,
    table: &str,
    socket_owner: NftSocketOwner,
    device: &str,
) -> Result<String, String> {
    use serde_json::json;
    if !crate::routing::is_safe_identifier(device) {
        return Err("speedtest-egress-device-invalid".into());
    }
    let mut batch: serde_json::Value =
        serde_json::from_str(batch).map_err(|_| "speedtest-egress-batch-invalid")?;
    let commands = batch["nftables"]
        .as_array_mut()
        .ok_or("speedtest-egress-batch-invalid")?;
    // POSTROUTING observes the route after OUTPUT mark/reroute and NAT. Do not
    // redirect traffic or accept a matching /0 label as an egress guarantee.
    commands.push(json!({"add":{"chain":{"family":"inet","table":table,
        "name":"egress","type":"filter","hook":"postrouting","prio":300,"policy":"accept"}}}));
    let uid = match socket_owner {
        NftSocketOwner::BackendUid(uid) => uid,
        NftSocketOwner::ProbeRootGid(_) => 0,
    };
    let mut owned = vec![json!({"match":{"op":"==","left":{"meta":{"key":"skuid"}},"right":uid}})];
    if let NftSocketOwner::ProbeRootGid(gid) = socket_owner {
        owned.push(json!({"match":{"op":"==","left":{"meta":{"key":"skgid"}},"right":gid}}));
    }
    // Existing local resolver allowance is DNS-only. This does not attest the
    // resolver daemon's subsequent upstream path; that remains separate scope.
    for (protocol, address) in [("ip", "127.0.0.1"), ("ip6", "::1")] {
        let mut dns = owned.clone();
        dns.extend([
            json!({"match":{"op":"==","left":{"meta":{"key":"oifname"}},"right":"lo"}}),
            json!({"match":{"op":"==","left":{"payload":{"protocol":protocol,"field":"daddr"}},"right":address}}),
            json!({"match":{"op":"in","left":{"meta":{"key":"l4proto"}},"right":{"set":["tcp","udp"]}}}),
            json!({"match":{"op":"==","left":{"ct":{"key":"proto-dst","dir":"original"}},"right":53}}),
            json!({"return":null}),
        ]);
        commands.push(
            json!({"add":{"rule":{"family":"inet","table":table,"chain":"egress","expr":dns}}}),
        );
    }
    let wrong_device =
        json!({"match":{"op":"!=","left":{"meta":{"key":"oifname"}},"right":device}});
    let deny = [
        json!({"counter":ACCOUNTING_FAULT_COUNTER}),
        json!({"drop":null}),
    ];
    owned.push(wrong_device.clone());
    owned.extend(deny.clone());
    commands.push(
        json!({"add":{"rule":{"family":"inet","table":table,"chain":"egress","expr":owned}}}),
    );
    // Kernel retransmissions may outlive the socket. The OUTPUT ownership
    // ledger clears reused tuples before membership is relied on here.
    let late = vec![
        json!({"match":{"op":"==","left":{"meta":{"key":"nfproto"}},"right":"ipv4"}}),
        json!({"match":{"op":"==","left":{"ct":{"key":"zone"}},"right":0}}),
        json!({"match":{"op":"==","left":{"ct":{"key":"direction"}},"right":"original"}}),
        json!({"match":{"op":"==","left":{"concat":[
            {"ct":{"key":"ip saddr","dir":"original"}},
            {"ct":{"key":"ip daddr","dir":"original"}},
            {"ct":{"key":"protocol"}},
            {"ct":{"key":"proto-src","dir":"original"}},
            {"ct":{"key":"proto-dst","dir":"original"}}
        ]},"right":"@owned_flows"}}),
        wrong_device,
        deny[0].clone(),
        deny[1].clone(),
    ];
    commands
        .push(json!({"add":{"rule":{"family":"inet","table":table,"chain":"egress","expr":late}}}));
    Ok(format!("{batch}\n"))
}

// Terse snapshots exclude flow elements to bound ownership/counter reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OwnedProbeCounters {
    pub(crate) rx_bytes: u64,
    pub(crate) tx_bytes: u64,
}

/// Permanent owners only use the current flow-accounting format. A legacy
/// counter or a same-name replacement must never become subtraction evidence.
pub(crate) fn attest_owned_probe_counters(
    input: &[u8],
    table: &str,
    owner: &str,
    handle: u64,
) -> Result<OwnedProbeCounters, String> {
    if attest_route_pin_snapshot(input, table, owner)? != handle {
        return Err("permanent-probe-counter-generation-changed".into());
    }
    let value = parse_nft_snapshot(input)?;
    let entries = value["nftables"]
        .as_array()
        .ok_or_else(|| "permanent-probe-counters-invalid".to_string())?;
    let counter_owner = format!("{owner}{FLOW_ACCOUNTING_OWNER_SUFFIX}");
    let mut counters = [None; 3];
    for entry in entries {
        let Some(counter) = entry.get("counter") else {
            continue;
        };
        let slot = match counter.get("name").and_then(serde_json::Value::as_str) {
            Some(ACCOUNTING_RX_COUNTER) => 0,
            Some(ACCOUNTING_TX_COUNTER) => 1,
            Some(ACCOUNTING_FAULT_COUNTER) => 2,
            _ => return Err("permanent-probe-counter-name-invalid".into()),
        };
        if entry.as_object().is_none_or(|object| object.len() != 1)
            || counter["family"].as_str() != Some("inet")
            || counter["table"].as_str() != Some(table)
            || counter["comment"].as_str() != Some(counter_owner.as_str())
        {
            return Err("permanent-probe-counter-owner-mismatch".into());
        }
        let bytes = counter["bytes"]
            .as_u64()
            .ok_or_else(|| "permanent-probe-counter-bytes-invalid".to_string())?;
        if counters[slot].replace(bytes).is_some() {
            return Err("permanent-probe-counter-duplicate".into());
        }
    }
    let [Some(rx_bytes), Some(tx_bytes), Some(faults)] = counters else {
        return Err("permanent-probe-counter-missing".into());
    };
    if faults != 0 {
        return Err("permanent-probe-flow-accounting-failed".into());
    }
    Ok(OwnedProbeCounters { rx_bytes, tx_bytes })
}

pub(crate) fn nft_table_snapshot_arguments(table: &str) -> [&str; 6] {
    ["-j", "-t", "list", "table", "inet", table]
}

fn parse_nft_snapshot(input: &[u8]) -> Result<serde_json::Value, String> {
    let invalid = || "speedtest-route-pin-json-invalid".to_string();
    if input.len() > MAX_NFT_SNAPSHOT_BYTES {
        return Err(invalid());
    }
    let value: serde_json::Value = serde_json::from_slice(input).map_err(|_| invalid())?;
    // Value keeps the last duplicate key. An absence proof must not silently
    // discard an earlier table list/name, including escaped duplicate keys.
    // JSON syntax is already checked above; scan only object key boundaries.
    let mut scopes: Vec<Vec<String>> = Vec::with_capacity(8);
    let mut index = 0;
    while index < input.len() {
        match input[index] {
            b'{' => {
                if scopes.len() == 16 {
                    return Err(invalid());
                }
                scopes.push(Vec::with_capacity(8));
                index += 1;
            }
            b'}' => {
                scopes.pop().ok_or_else(invalid)?;
                index += 1;
            }
            b'"' => {
                let start = index;
                index += 1;
                while index < input.len() && input[index] != b'"' {
                    index += if input[index] == b'\\' { 2 } else { 1 };
                }
                index += 1;
                let mut next = index;
                while input.get(next).is_some_and(u8::is_ascii_whitespace) {
                    next += 1;
                }
                if input.get(next) == Some(&b':') {
                    let key: String =
                        serde_json::from_slice(&input[start..index]).map_err(|_| invalid())?;
                    let keys = scopes.last_mut().ok_or_else(invalid)?;
                    if key.len() > 128 || keys.len() == 64 || keys.contains(&key) {
                        return Err(invalid());
                    }
                    keys.push(key);
                }
            }
            _ => index += 1,
        }
    }
    Ok(value)
}

pub(crate) fn attest_route_pin_snapshot(
    input: &[u8],
    expected_table: &str,
    expected_owner: &str,
) -> Result<u64, String> {
    let value = parse_nft_snapshot(input)?;
    let mismatch = || "speedtest-route-pin-owner-mismatch".to_string();
    if expected_table.is_empty() || expected_owner.is_empty() {
        return Err(mismatch());
    }
    let root = value
        .as_object()
        .filter(|root| root.len() == 1)
        .ok_or_else(mismatch)?;
    let entries = root
        .get("nftables")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(mismatch)?;
    let mut handle = None;
    for entry in entries {
        let entry = entry
            .as_object()
            .filter(|entry| entry.len() == 1)
            .ok_or_else(mismatch)?;
        let Some(table) = entry.get("table") else {
            continue;
        };
        if handle.is_some()
            || table.get("family").and_then(serde_json::Value::as_str) != Some("inet")
            || table.get("name").and_then(serde_json::Value::as_str) != Some(expected_table)
            || table.get("comment").and_then(serde_json::Value::as_str) != Some(expected_owner)
        {
            return Err(mismatch());
        }
        handle = Some(
            table
                .get("handle")
                .and_then(serde_json::Value::as_u64)
                .filter(|handle| *handle != 0)
                .ok_or("speedtest-route-pin-handle-invalid")?,
        );
    }
    handle.ok_or_else(mismatch)
}

pub(crate) fn cleanup_named_route_pin_with(
    table: &str,
    owner: &str,
    mut execute: impl FnMut(&[&str]) -> Result<(bool, Vec<u8>), String>,
) -> Result<(), String> {
    let listed = execute(&nft_table_snapshot_arguments(table))?;
    if !listed.0 {
        let tables = execute(&["-j", "list", "tables"])?;
        if tables.0 && nft_table_snapshot_proves_absence(&tables.1, table)? {
            return Ok(());
        }
        return Err("speedtest-route-pin-inspection-failed".to_string());
    }
    let handle = attest_route_pin_snapshot(&listed.1, table, owner)?.to_string();
    // A name can be reused after inspection. Only delete the exact kernel
    // object we attested, never a replacement under the same table name.
    let deleted = execute(&["delete", "table", "inet", "handle", &handle])?;
    if !deleted.0 {
        return Err("speedtest-route-pin-delete-failed".to_string());
    }
    if execute(&nft_table_snapshot_arguments(table))?.0 {
        return Err("speedtest-route-pin-delete-unverified".to_string());
    }
    let tables = execute(&["-j", "list", "tables"])?;
    if !tables.0 || !nft_table_snapshot_proves_absence(&tables.1, table)? {
        return Err("speedtest-route-pin-delete-unverified".to_string());
    }
    Ok(())
}

pub(crate) fn nft_table_snapshot_proves_absence(
    input: &[u8],
    expected_table: &str,
) -> Result<bool, String> {
    let invalid = || "speedtest-route-pin-tables-json-invalid".to_string();
    let value = parse_nft_snapshot(input).map_err(|_| invalid())?;
    let root = value
        .as_object()
        .filter(|root| root.len() == 1)
        .ok_or_else(invalid)?;
    let entries = root
        .get("nftables")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(invalid)?;
    for entry in entries {
        let entry = entry
            .as_object()
            .filter(|entry| entry.len() == 1)
            .ok_or_else(invalid)?;
        if let Some(table) = entry.get("table") {
            let family = table
                .get("family")
                .and_then(serde_json::Value::as_str)
                .filter(|v| !v.is_empty())
                .ok_or_else(invalid)?;
            let name = table
                .get("name")
                .and_then(serde_json::Value::as_str)
                .filter(|v| !v.is_empty())
                .ok_or_else(invalid)?;
            if family == "inet" && name == expected_table {
                return Ok(false);
            }
        } else if !entry
            .get("metainfo")
            .is_some_and(serde_json::Value::is_object)
        {
            return Err(invalid());
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    #[test]
    fn r6_permanent_counters_require_exact_generation_and_complete_flow_evidence() {
        use super::*;
        use serde_json::json;
        let mut entries = vec![json!({"table":{
            "family":"inet", "name":"cake_pm_42", "comment":"owner", "handle":42
        }})];
        for (name, bytes) in [
            (ACCOUNTING_RX_COUNTER, 12),
            (ACCOUNTING_TX_COUNTER, 34),
            (ACCOUNTING_FAULT_COUNTER, 0),
        ] {
            entries.push(json!({"counter":{
                "family":"inet", "table":"cake_pm_42", "comment":"owner:flow-v1",
                "name":name, "bytes":bytes, "packets":0
            }}));
        }
        let valid = json!({"nftables":entries});
        let read = |value: &serde_json::Value| {
            attest_owned_probe_counters(
                &serde_json::to_vec(value).unwrap(),
                "cake_pm_42",
                "owner",
                42,
            )
        };
        assert_eq!(
            read(&valid).unwrap(),
            OwnedProbeCounters {
                rx_bytes: 12,
                tx_bytes: 34
            }
        );
        for (index, field, replacement) in [
            (0, "handle", json!(43)),
            (0, "comment", json!("foreign")),
            (1, "family", json!("ip")),
            (1, "table", json!("foreign")),
            (1, "comment", json!("owner")), // No legacy fallback for permanent probes.
            (2, "comment", json!("foreign:flow-v1")),
            (3, "bytes", json!(1)),
            (1, "bytes", json!(-1)),
            (1, "bytes", json!(1.5)),
            (1, "bytes", json!("12")),
        ] {
            let mut bad = valid.clone();
            let kind = if index == 0 { "table" } else { "counter" };
            bad["nftables"][index][kind][field] = replacement;
            assert!(read(&bad).is_err(), "accepted invalid {index}/{field}");
        }
        for index in 1..=3 {
            let mut missing = valid.clone();
            missing["nftables"].as_array_mut().unwrap().remove(index);
            assert!(read(&missing).is_err());
            let mut duplicate = valid.clone();
            duplicate["nftables"]
                .as_array_mut()
                .unwrap()
                .push(valid["nftables"][index].clone());
            assert!(read(&duplicate).is_err());
        }
        let encoded = serde_json::to_string(&valid).unwrap();
        let duplicate_bytes = encoded.replacen("\"bytes\":12", "\"bytes\":12,\"bytes\":0", 1);
        assert_ne!(encoded, duplicate_bytes);
        assert!(
            attest_owned_probe_counters(duplicate_bytes.as_bytes(), "cake_pm_42", "owner", 42)
                .is_err()
        );
        let mut max = valid.clone();
        max["nftables"][1]["counter"]["bytes"] = json!(u64::MAX);
        assert_eq!(read(&max).unwrap().rx_bytes, u64::MAX);
    }

    use super::*;

    #[test]
    fn r6_cleanup_requires_handle_and_never_retries_by_name() {
        for handle in ["null", "0", "-1", "1.5", "\"42\"", "18446744073709551616"] {
            let input = format!(
                r#"{{"nftables":[{{"table":{{"family":"inet","name":"target","comment":"owner","handle":{handle}}}}}]}}"#
            );
            assert!(attest_route_pin_snapshot(input.as_bytes(), "target", "owner").is_err());
        }
        let original = br#"{"nftables":[{"table":{"family":"inet","name":"target","comment":"owner","handle":42}}]}"#;
        let mut calls = 0;
        let error = cleanup_named_route_pin_with("target", "owner", |args| {
            calls += 1;
            match calls {
                1 => Ok((true, original.to_vec())),
                2 => {
                    assert_eq!(args, ["delete", "table", "inet", "handle", "42"]);
                    // Original gone; a same-name foreign replacement must not
                    // cause a retry by name or adoption of its new handle.
                    Ok((false, Vec::new()))
                }
                _ => panic!("failed exact deletion must not fall back"),
            }
        })
        .unwrap_err();
        assert_eq!(calls, 2);
        assert_eq!(error, "speedtest-route-pin-delete-failed");
        assert_eq!(
            attest_route_pin_snapshot(original, "target", "owner").unwrap(),
            42
        );
    }

    #[test]
    #[ignore = "requires a fresh isolated network namespace and explicit nft path"]
    fn r6_cleanup_handle_kernel_fixture() {
        let nft = std::env::var_os("CAKE_R6_CLEANUP_NFT").expect("explicit fixture nft");
        let parent = std::env::var_os("CAKE_R6_PARENT_NETNS").expect("parent namespace");
        assert_ne!(
            std::fs::read_link("/proc/self/ns/net").unwrap().as_os_str(),
            parent
        );
        let run = |args: &[&str]| {
            let output = std::process::Command::new(&nft)
                .args(args)
                .output()
                .unwrap();
            (output.status.success(), output.stdout)
        };
        let (ok, empty) = run(&["-j", "list", "tables"]);
        assert!(ok && nft_table_snapshot_proves_absence(&empty, "cake_cleanup").unwrap());
        let create = [
            "add",
            "table",
            "inet",
            "cake_cleanup",
            "{ comment \"owner\"; }",
        ];
        assert!(run(&create).0);
        cleanup_named_route_pin_with("cake_cleanup", "owner", |args| Ok(run(args))).unwrap();
        assert!(run(&create).0);
        let mut replaced = false;
        let result = cleanup_named_route_pin_with("cake_cleanup", "owner", |args| {
            if args.first() == Some(&"delete") {
                assert_eq!(&args[..4], ["delete", "table", "inet", "handle"]);
                assert!(!replaced);
                assert!(run(&["delete", "table", "inet", "cake_cleanup"]).0);
                assert!(
                    run(&[
                        "add",
                        "table",
                        "inet",
                        "cake_cleanup",
                        "{ comment \"foreign\"; }"
                    ])
                    .0
                );
                replaced = true;
            }
            Ok(run(args))
        });
        assert_eq!(result.unwrap_err(), "speedtest-route-pin-delete-failed");
        assert!(replaced);
        let (ok, replacement) = run(&nft_table_snapshot_arguments("cake_cleanup"));
        assert!(ok);
        assert!(attest_route_pin_snapshot(&replacement, "cake_cleanup", "foreign").is_ok());
        // Explicitly clean only the replacement created by this fixture.
        cleanup_named_route_pin_with("cake_cleanup", "foreign", |args| Ok(run(args))).unwrap();
        let (ok, final_tables) = run(&["-j", "list", "tables"]);
        assert!(ok && nft_table_snapshot_proves_absence(&final_tables, "cake_cleanup").unwrap());
        // Exercise the actual installation batch, including egress rules.
        for socket_owner in [
            NftSocketOwner::BackendUid(1234),
            NftSocketOwner::ProbeRootGid(42),
        ] {
            let install = || {
                install_owned_route_pin_with(
                    "cake_cleanup",
                    "owner",
                    socket_owner,
                    Some((!0x3f00, 0x200)),
                    "lo",
                    |arguments, input| {
                        use std::io::Write;
                        use std::process::Stdio;
                        let mut child = std::process::Command::new(&nft)
                            .args(arguments)
                            .stdin(Stdio::piped())
                            .stdout(Stdio::piped())
                            .stderr(Stdio::piped())
                            .spawn()
                            .unwrap();
                        child.stdin.take().unwrap().write_all(input).unwrap();
                        Ok(child.wait_with_output().unwrap().status.success())
                    },
                )
            };
            install().unwrap();
            cleanup_named_route_pin_with("cake_cleanup", "owner", |args| Ok(run(args))).unwrap();
            assert!(
                run(&[
                    "create",
                    "table",
                    "inet",
                    "cake_cleanup",
                    "{ comment \"foreign\"; }"
                ])
                .0
            );
            let (ok, before) = run(&["-j", "list", "table", "inet", "cake_cleanup"]);
            assert!(ok);
            assert!(install().is_err(), "must not adopt an existing table");
            let (ok, after) = run(&["-j", "list", "table", "inet", "cake_cleanup"]);
            assert!(ok);
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&before).unwrap(),
                serde_json::from_slice::<serde_json::Value>(&after).unwrap(),
                "failed batch modified foreign table"
            );
            cleanup_named_route_pin_with("cake_cleanup", "foreign", |args| Ok(run(args))).unwrap();
        }
    }

    #[test]
    fn r6_cleanup_requires_exact_table_owner_not_first_comment() {
        for input in [
            r#"{"nftables":[{"counter":{"comment":"owner"}}]}"#,
            r#"{"nftables":[{"counter":{"comment":"owner"}},{"table":{"family":"inet","name":"target","comment":"foreign"}}]}"#,
            r#"{"nftables":[{"table":{"family":"inet","name":"other","comment":"owner"}}]}"#,
            r#"{"nftables":[{"table":{"family":"ip","name":"target","comment":"owner"}}]}"#,
            r#"{"nftables":[{"table":{"family":"inet","name":"target","comment":"owner","\u0063omment":"foreign"}}]}"#,
            r#"{"nftables":[{"table":{"family":"inet","name":"target","comment":"owner","handle":42}},{"table":{"family":"inet","name":"target","comment":"owner","handle":42}}]}"#,
            r#"{"nftables":[{"table":{"family":"inet","name":"target","comment":"owner"}}],"nftables":[]}"#,
            r#"{"nftables":[{"table":{"family":"inet","name":"target","comment":"owner"}}]"#,
        ] {
            assert!(attest_route_pin_snapshot(input.as_bytes(), "target", "owner").is_err());
            let mut calls = 0;
            let result = cleanup_named_route_pin_with("target", "owner", |args| {
                calls += 1;
                assert_eq!(calls, 1, "uncertain ownership must never issue delete");
                assert_eq!(args, nft_table_snapshot_arguments("target"));
                Ok((true, input.as_bytes().to_vec()))
            });
            assert!(result.is_err(), "{input}");
            assert_eq!(calls, 1);
        }
        // nft objects may appear before the table; their comments are not
        // the table's ownership, regardless of ordering.
        let valid = br#"{"nftables":[{"metainfo":{"json_schema_version":1}},{"counter":{"comment":"different"}},{"table":{"family":"inet","name":"target","comment":"owner","handle":42}}]}"#;
        assert_eq!(
            attest_route_pin_snapshot(valid, "target", "owner").unwrap(),
            42
        );
        assert!(attest_route_pin_snapshot(valid, "", "owner").is_err());
        assert!(attest_route_pin_snapshot(valid, "target", "").is_err());
    }

    #[test]
    fn t2_route_pin_cleanup_requires_positive_absence_evidence() {
        let table = "cake_st_aaaaaaaaaaaa_bbbbbbbbbbbb";
        let owner = "cake-autorate-speedtest:test-owner";
        let present = format!(r#"{{"nftables":[{{"table":{{"family":"inet","name":"{table}","comment":"{owner}","handle":42}}}}]}}"#).into_bytes();
        let absent = br#"{"nftables":[{"metainfo":{"json_schema_version":1}},{"table":{"family":"ip","name":"foreign"}}]}"#.to_vec();
        for (snapshot, succeeds) in [
            (present.clone(), false),
            (absent.clone(), true),
            (b"{}".to_vec(), false),
        ] {
            let mut calls = 0;
            let result = cleanup_named_route_pin_with(table, owner, |arguments| {
                calls += 1;
                match calls {
                    1 => {
                        assert_eq!(arguments, nft_table_snapshot_arguments(table));
                        Ok((false, Vec::new()))
                    }
                    2 => {
                        assert_eq!(arguments, ["-j", "list", "tables"]);
                        Ok((true, snapshot.clone()))
                    }
                    _ => panic!("uncertain initial ownership attempted deletion"),
                }
            });
            assert_eq!(result.is_ok(), succeeds);
            assert_eq!(calls, 2);
        }
        for (remaining, succeeds) in [(present.clone(), false), (absent, true)] {
            let mut calls = 0;
            let result = cleanup_named_route_pin_with(table, owner, |arguments| {
                calls += 1;
                match calls {
                    1 => Ok((true, present.clone())),
                    2 => {
                        assert_eq!(arguments, ["delete", "table", "inet", "handle", "42"]);
                        Ok((true, Vec::new()))
                    }
                    3 => Ok((false, Vec::new())),
                    4 => {
                        assert_eq!(arguments, ["-j", "list", "tables"]);
                        Ok((true, remaining.clone()))
                    }
                    _ => panic!("cleanup retried without new authority"),
                }
            });
            assert_eq!(result.is_ok(), succeeds);
            assert_eq!(calls, 4);
        }
        let mut calls = 0;
        assert!(cleanup_named_route_pin_with(table, "different-owner", |_| {
            calls += 1;
            assert_eq!(calls, 1, "foreign table must not be deleted");
            Ok((true, present.clone()))
        })
        .unwrap_err()
        .contains("owner-mismatch"));
    }

    #[test]
    fn t2_route_pin_absence_rejects_ambiguous_or_malformed_json() {
        assert!(nft_table_snapshot_proves_absence(br#"{"nftables":[]}"#, "target").unwrap());
        assert!(!nft_table_snapshot_proves_absence(
            br#"{"nftables":[{"table":{"family":"inet","name":"target"}}]}"#,
            "target"
        )
        .unwrap());
        assert!(nft_table_snapshot_proves_absence(
            br#"{"nftables":[{"table":{"family":"ip","name":"target"}}]}"#,
            "target"
        )
        .unwrap());
        for invalid in [
            r#"{}"#,
            r#"{"nftables":null}"#,
            r#"{"nftables":[],"extra":1}"#,
            r#"{"nftables":[{"table":{"name":"other"}}]}"#,
            r#"{"nftables":[{"add":{"table":{"family":"inet","name":"target"}}}]}"#,
            r#"{"nftables":[{"table":{"family":"inet","name":"target"}}],"nftables":[]}"#,
            r#"{"nftables":[{"table":{"family":"inet","name":"target","\u006eame":"other"}}]}"#,
            r#"{"nftables":[{"table":{"family":"inet","name":"target"},"metainfo":{}}]}"#,
            r#"{"nftables":[]"#,
        ] {
            assert!(
                nft_table_snapshot_proves_absence(invalid.as_bytes(), "target").is_err(),
                "{invalid}"
            );
        }
        assert!(nft_table_snapshot_proves_absence(
            &vec![b' '; MAX_NFT_SNAPSHOT_BYTES + 1],
            "target"
        )
        .is_err());
        assert!(nft_table_snapshot_proves_absence(br#"{"nftables":[{"table":{"family":"inet","name":"quoted\"{name}","comment":"a\\b"}}]}"#, "target").unwrap());
    }

    #[test]
    fn r6_egress_guard_covers_backend_probes_and_late_owned_flows() {
        use super::*;
        for socket_owner in [
            NftSocketOwner::BackendUid(1234),
            NftSocketOwner::ProbeRootGid(5678),
        ] {
            let base = nft_owned_route_pin_batch("cake_test", "owner", socket_owner, None);
            let guarded =
                nft_egress_guard_batch(&base, "cake_test", socket_owner, "wan-test").unwrap();
            let value: serde_json::Value = serde_json::from_str(&guarded).unwrap();
            let rows = value["nftables"].as_array().unwrap();
            let chain = rows
                .iter()
                .find_map(|row| row.pointer("/add/chain").filter(|c| c["name"] == "egress"))
                .unwrap();
            assert_eq!(chain["hook"], "postrouting");
            assert_eq!(chain["prio"], 300);
            let rules: Vec<_> = rows
                .iter()
                .filter_map(|r| r.pointer("/add/rule"))
                .filter(|r| r["chain"] == "egress")
                .collect();
            assert_eq!(rules.len(), 4);
            for dns in &rules[..2] {
                let text = dns.to_string();
                assert!(
                    text.contains("daddr") && text.contains("proto-dst") && text.contains("53")
                );
                assert!(text.contains("lo") && text.contains("skuid"));
            }
            for deny in &rules[2..] {
                let text = deny.to_string();
                assert!(text.contains("wan-test") && text.contains("!=") && text.contains("drop"));
                assert!(text.contains(ACCOUNTING_FAULT_COUNTER));
            }
            assert!(rules[3].to_string().contains("@owned_flows"));
            assert!(!rules[3].to_string().contains("skuid"));
            if matches!(socket_owner, NftSocketOwner::ProbeRootGid(_)) {
                assert!(rules[2].to_string().contains("skgid"));
            }
            assert!(nft_egress_guard_batch(&base, "cake_test", socket_owner, "wan;bad").is_err());
        }
    }
    #[test]
    fn t2_dns6_is_bounded_loopback_dns_not_an_ipv6_wan_bypass() {
        for owner in [
            NftSocketOwner::BackendUid(32769),
            NftSocketOwner::ProbeRootGid(32770),
        ] {
            let batch: serde_json::Value = serde_json::from_str(&nft_owned_route_pin_batch(
                "test",
                "owner",
                owner,
                Some((!0x3f00, 0x200)),
            ))
            .unwrap();
            let commands = batch["nftables"].as_array().unwrap();
            let set = commands
                .iter()
                .find(|row| row["add"]["set"]["name"] == "owned_dns6")
                .unwrap();
            assert_eq!(set["add"]["set"]["size"], MAX_ACCOUNTING_FLOWS);
            assert!(set["add"]["set"].get("timeout").is_none());
            let rules: Vec<_> = commands
                .iter()
                .filter_map(|row| row["add"].get("rule"))
                .filter(|rule| rule.to_string().contains("owned_dns6"))
                .collect();
            assert!(rules.len() >= 4);
            for rule in rules {
                let text = rule.to_string();
                assert!(text.contains("ip6 saddr") && text.contains("ip6 daddr"));
                assert!(text.contains("\"right\":\"::1\"") && text.contains("\"right\":53"));
                assert!(text.contains("\"right\":\"lo\"") && text.contains("\"zone\""));
                assert!(text.contains("\"tcp\"") && text.contains("\"udp\""));
                assert!(!text.contains("mangle"));
            }
        }
    }

    #[test]
    fn r6_shared_rules_preserve_accepted_batch_except_exclusive_creation() {
        use sha2::{Digest, Sha256};
        // Exact batches exercised by the kernel and real-backend fixtures,
        // before extracting this shared renderer. UID0 is fixture-only.
        for (mark, expected) in [
            (
                None,
                "e178672d5e0f7cb56305e589a5b0b94e1bff528067cdc1590d8f1f4db7cc0684",
            ),
            (
                Some((!0x3f00, 0x200)),
                "dc2cceccd0209c4e5ccccd4cd8dd37d935783e10a79f1796d485cc38bfc469ea",
            ),
        ] {
            let owner = NftSocketOwner::BackendUid(0);
            let base = nft_owned_route_pin_batch("cake_r6_guard", "isolated-test", owner, mark);
            let batch = nft_egress_guard_batch(&base, "cake_r6_guard", owner, "cake_test").unwrap();
            let mut value: serde_json::Value = serde_json::from_str(&batch).unwrap();
            let first = &mut value["nftables"][0];
            let table = first["create"]["table"].clone();
            assert_eq!(table["name"], "cake_r6_guard");
            assert!(first.get("add").is_none());
            *first = serde_json::json!({"add":{"table":table}});
            // Keep the original golden proof for every other byte/field;
            // actual exclusive creation is exercised by the kernel fixture.
            let legacy = format!("{}\n", serde_json::to_string(&value).unwrap());
            assert_eq!(format!("{:x}", Sha256::digest(legacy.as_bytes())), expected);
        }
    }

    #[test]
    fn r6_shared_install_is_one_stdin_batch_and_propagates_failure() {
        for result in [
            Ok(true),
            Ok(false),
            Err("bounded-command-timeout".to_string()),
        ] {
            let mut calls = 0;
            let installed = install_owned_route_pin_with(
                "owned",
                "owner",
                NftSocketOwner::BackendUid(1234),
                None,
                "wan",
                |args, input| {
                    calls += 1;
                    assert_eq!(args, ["-j", "-f", "-"]);
                    let value: serde_json::Value = serde_json::from_slice(input).unwrap();
                    assert_eq!(value["nftables"][0]["create"]["table"]["comment"], "owner");
                    result.clone()
                },
            );
            assert_eq!(calls, 1);
            match result {
                Ok(true) => installed.unwrap(),
                Ok(false) => {
                    assert_eq!(installed.unwrap_err(), "speedtest-route-pin-install-failed")
                }
                Err(error) => assert_eq!(installed.unwrap_err(), error),
            }
        }
        assert!(install_owned_route_pin_with(
            "owned",
            "owner",
            NftSocketOwner::BackendUid(1234),
            None,
            "bad;device",
            |_, _| panic!("invalid device reached executor")
        )
        .is_err());
    }
}
