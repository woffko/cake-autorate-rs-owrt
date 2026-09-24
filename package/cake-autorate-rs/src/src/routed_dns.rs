//! Bounded A-query wire codec for the route-owned TCP resolver.
//! RFC 1035 sections 4.1/4.2.2; no system-resolver fallback or shared cache.
use std::net::Ipv4Addr;

pub(crate) const MAX_PACKET: usize = 4096;
const MAX_RECORDS: usize = 64;
const INVALID: &str = "routed-dns-invalid-response";

pub(crate) fn query(host: &str, id: u16) -> Result<Vec<u8>, String> {
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.is_empty() || host.len() > 253 {
        return Err("routed-dns-invalid-host".into());
    }
    let mut packet = Vec::with_capacity(272);
    packet.extend_from_slice(&id.to_be_bytes());
    packet.extend_from_slice(&[1, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
    for label in host.split('.') {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err("routed-dns-invalid-host".into());
        }
        packet.push(label.len() as u8);
        packet.extend(label.bytes().map(|b| b.to_ascii_lowercase()));
    }
    packet.extend_from_slice(&[0, 0, 1, 0, 1]);
    Ok(packet)
}

fn word(packet: &[u8], offset: usize) -> Result<u16, String> {
    let bytes = packet.get(offset..offset + 2).ok_or(INVALID)?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

// Expanded wire names preserve label boundaries and normalize ASCII case.
// Backward-only pointers and a hop bound reject cycles and pathological work.
fn name(packet: &[u8], cursor: &mut usize) -> Result<Vec<u8>, String> {
    let mut at = *cursor;
    let mut jumped = false;
    let mut result = Vec::with_capacity(255);
    for _ in 0..128 {
        let size = *packet.get(at).ok_or(INVALID)?;
        if size & 0xc0 == 0xc0 {
            let target = usize::from(word(packet, at)? & 0x3fff);
            if target < 12 || target >= at {
                return Err(INVALID.into());
            }
            if !jumped {
                *cursor = at + 2;
            }
            jumped = true;
            at = target;
        } else if size <= 63 {
            at += 1;
            if result.len() + usize::from(size) + 1 > 255 {
                return Err(INVALID.into());
            }
            result.push(size);
            let label = packet.get(at..at + usize::from(size)).ok_or(INVALID)?;
            result.extend(label.iter().map(u8::to_ascii_lowercase));
            at += usize::from(size);
            if !jumped {
                *cursor = at;
            }
            if size == 0 {
                return Ok(result);
            }
        } else {
            return Err(INVALID.into());
        }
    }
    Err(INVALID.into())
}

pub(crate) fn answer(packet: &[u8], request: &[u8]) -> Result<Vec<Ipv4Addr>, String> {
    if !(12..=MAX_PACKET).contains(&packet.len()) || request.len() < 17
        || word(packet, 0)? != word(request, 0)?
        // QR, standard opcode, no truncation, reserved Z clear, successful RCODE.
        || word(packet, 2)? & 0xfa4f != 0x8000 || word(packet, 4)? != 1
    {
        return Err(INVALID.into());
    }
    let counts = [word(packet, 6)?, word(packet, 8)?, word(packet, 10)?];
    let total: usize = counts.iter().map(|n| usize::from(*n)).sum();
    if total > MAX_RECORDS {
        return Err(INVALID.into());
    }
    let mut at = 12;
    let question = name(packet, &mut at)?;
    if question != request[12..request.len() - 4] || packet.get(at..at + 4) != Some(&[0, 1, 0, 1]) {
        return Err(INVALID.into());
    }
    at += 4;
    let mut addresses = Vec::with_capacity(usize::from(counts[0]));
    let mut aliases = Vec::with_capacity(usize::from(counts[0]));
    for index in 0..total {
        let owner = name(packet, &mut at)?;
        let kind = word(packet, at)?;
        let class = word(packet, at + 2)?;
        let length = usize::from(word(packet, at + 8)?);
        at += 10;
        let end = at
            .checked_add(length)
            .filter(|end| *end <= packet.len())
            .ok_or(INVALID)?;
        if index < usize::from(counts[0]) && class == 1 {
            match kind {
                1 => {
                    if length != 4 {
                        return Err(INVALID.into());
                    }
                    addresses.push((
                        owner,
                        Ipv4Addr::new(packet[at], packet[at + 1], packet[at + 2], packet[at + 3]),
                    ));
                }
                5 => {
                    let mut cursor = at;
                    let target = name(packet, &mut cursor)?;
                    if cursor != end {
                        return Err(INVALID.into());
                    }
                    aliases.push((owner, target));
                }
                _ => {}
            }
        }
        at = end;
    }
    if at != packet.len() {
        return Err(INVALID.into());
    }
    let mut current = question;
    let mut seen = Vec::with_capacity(9);
    for _ in 0..9 {
        if seen.contains(&current) {
            return Err(INVALID.into());
        }
        seen.push(current.clone());
        let mut matches = aliases.iter().filter(|(owner, _)| owner == &current);
        let next = matches.next();
        if let Some((_, target)) = next {
            if matches.any(|(_, other)| other != target)
                || addresses.iter().any(|(owner, _)| owner == &current)
            {
                return Err(INVALID.into());
            }
            current = target.clone();
        } else {
            let mut result: Vec<_> = addresses
                .iter()
                .filter(|(owner, _)| owner == &current)
                .map(|(_, ip)| *ip)
                .collect();
            result.sort_unstable();
            result.dedup();
            if result.is_empty() || result.len() > 32 {
                return Err("routed-dns-no-bounded-answer".into());
            }
            return Ok(result);
        }
    }
    Err("routed-dns-alias-limit".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compressed_answer_and_malformed_frames() {
        let query = query("Example.test.", 42).unwrap();
        let mut response = query.clone();
        response[2..4].copy_from_slice(&[0x81, 0x80]);
        response[6..8].copy_from_slice(&[0, 1]);
        response.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 1, 0, 4, 192, 0, 2, 1]);
        assert_eq!(
            answer(&response, &query).unwrap(),
            [Ipv4Addr::new(192, 0, 2, 1)]
        );
        for length in 0..response.len() {
            assert!(answer(&response[..length], &query).is_err());
        }
        for (offset, value) in [
            (0, 1),
            (2, 0x83),
            (3, 0x83),
            (5, 2),
            (query.len(), 0xff),
            (query.len() + 1, 255),
        ] {
            let mut bad = response.clone();
            bad[offset] = value;
            assert!(answer(&bad, &query).is_err());
        }
        response.push(0);
        assert!(answer(&response, &query).is_err());
        for host in ["", "a..b", "-a.test", "a-.test", "bad/name"] {
            assert!(super::query(host, 1).is_err());
        }
    }

    #[test]
    fn cname_chain_only_accepts_related_answer_addresses() {
        let request = query("alias.test", 7).unwrap();
        let target = query("target.test", 7).unwrap();
        let target_name = &target[12..target.len() - 4];
        let mut response = request.clone();
        response[2..4].copy_from_slice(&[0x81, 0x80]);
        response[6..8].copy_from_slice(&[0, 2]);
        response.extend_from_slice(&[0xc0, 12, 0, 5, 0, 1, 0, 0, 0, 1]);
        response.extend_from_slice(&(target_name.len() as u16).to_be_bytes());
        response.extend_from_slice(target_name);
        response.extend_from_slice(target_name);
        response.extend_from_slice(&[0, 1, 0, 1, 0, 0, 0, 1, 0, 4, 192, 0, 2, 8]);
        assert_eq!(
            answer(&response, &request).unwrap(),
            [Ipv4Addr::new(192, 0, 2, 8)]
        );
        // Unrelated address in the answer section must not satisfy the alias.
        let owner_start = response.len() - 14 - target_name.len();
        response[owner_start + 1] = b'x';
        assert!(answer(&response, &request).is_err());
        // A compression pointer to itself must terminate with an error.
        let offset = request.len();
        response[offset] = 0xc0 | ((offset >> 8) as u8);
        response[offset + 1] = offset as u8;
        assert!(answer(&response, &request).is_err());
    }
}
