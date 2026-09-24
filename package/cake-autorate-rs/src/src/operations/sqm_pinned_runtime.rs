//! Retained-queue observations for an ordinary reload. Configuration/state
//! attestation remains authoritative; these witnesses additionally detect
//! observable state-file replacement, link recreation and qdisc handle drift.
use super::*;

pub(super) enum Inspection<'a> {
    Independent,
    Lifecycle(&'a super::super::service_lifecycle::ServiceGlobalLease<'a>),
}
impl Inspection<'_> {
    pub(super) fn attest(
        &self,
        paths: &OpenWrtPaths,
        target: &str,
    ) -> Result<(), NativeSqmAttestationError> {
        if let Self::Lifecycle(lease) = self {
            lease
                .attest_root(&paths.lock_root)
                .map_err(NativeSqmAttestationError::failed)?;
            // Every cooperating interface operation takes the global lock
            // first. Its exclusive owner needs no second shared/OFD lock, but
            // a crashed operation's recovery record still forbids adoption.
            let record = paths
                .lock_root
                .join(format!("interface-{}.lock", interface_lock_stem(target)?));
            match fs::symlink_metadata(&record) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => {
                    return Err(NativeSqmAttestationError::failed(
                        "retained SQM recovery record unavailable",
                    ))
                }
                Ok(_) => {
                    let record = parse_lock_record(&read_bounded_file(&record, 4096)?)?;
                    if !record.recovery_journal.is_empty()
                        && Path::new(&record.recovery_journal).is_file()
                    {
                        return Err(NativeSqmAttestationError::Busy(
                            "retained SQM has pending interface recovery".into(),
                        ));
                    }
                }
            }
        }
        Ok(())
    }
    fn claim(
        &self,
        paths: &OpenWrtPaths,
        target: &str,
    ) -> Result<Option<RuntimeLockClaim>, NativeSqmAttestationError> {
        self.attest(paths, target)?;
        match self {
            Self::Independent => acquire_runtime_locks(paths, target, "sqm-attest").map(Some),
            Self::Lifecycle(_) => Ok(None),
        }
    }
}

struct StateFile {
    _file: File, // Pin the inode until the parent transition finishes.
    identity: super::super::committed_uci::Identity,
    bytes: Vec<u8>,
}
#[derive(PartialEq, Eq)]
struct Device {
    ifindex: Option<u32>,
    qdiscs: Vec<(String, String, String)>,
}
struct Snapshot {
    state: Option<StateFile>,
    devices: BTreeMap<String, Device>,
    ingress: Option<Vec<u8>>,
}
impl Snapshot {
    fn same(&self, other: &Self) -> bool {
        let state = match (&self.state, &other.state) {
            (None, None) => true,
            (Some(a), Some(b)) => a.identity == b.identity && a.bytes == b.bytes,
            _ => false,
        };
        state && self.devices == other.devices && self.ingress == other.ingress
    }
    fn read(
        spec: &ManagedSqmAttestationSpec,
        paths: &OpenWrtPaths,
    ) -> Result<Self, NativeSqmAttestationError> {
        let state_path = paths
            .sqm_state_root
            .join(format!("{}.state", spec.target_interface));
        let state = if let Some(mut file) =
            open_optional_bounded_external_regular(&state_path, MAX_STATE_BYTES)?
        {
            let identity = super::super::committed_uci::file_identity(&state_path, &file, false)
                .map_err(NativeSqmAttestationError::failed)?;
            let mut bytes = Vec::new();
            (&mut file)
                .take((MAX_STATE_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|_| NativeSqmAttestationError::failed("retained SQM state read failed"))?;
            if bytes.len() > MAX_STATE_BYTES
                || super::super::committed_uci::file_identity(&state_path, &file, false)
                    .map_err(NativeSqmAttestationError::failed)?
                    != identity
            {
                return Err(NativeSqmAttestationError::failed(
                    "retained SQM state changed while reading",
                ));
            }
            Some(StateFile {
                _file: file,
                identity,
                bytes,
            })
        } else {
            None
        };
        let mut devices = BTreeMap::new();
        for name in BTreeSet::from([
            &spec.target_interface,
            &spec.upload_interface,
            &spec.download_interface,
        ]) {
            let directory = paths.sys_class_net.join(name);
            let device = if directory.exists() {
                let mut bytes = Vec::new();
                File::open(directory.join("ifindex"))
                    .and_then(|file| file.take(65).read_to_end(&mut bytes))
                    .map_err(|_| {
                        NativeSqmAttestationError::failed("retained SQM ifindex read failed")
                    })?;
                let ifindex = std::str::from_utf8(&bytes)
                    .ok()
                    .filter(|_| bytes.len() <= 64)
                    .and_then(|v| v.trim().parse::<u32>().ok())
                    .filter(|v| *v != 0)
                    .ok_or_else(|| {
                        NativeSqmAttestationError::failed("retained SQM ifindex invalid")
                    })?;
                let output = tc_output(paths, &["-details", "qdisc", "show", "dev", name])?;
                Device {
                    ifindex: Some(ifindex),
                    qdiscs: qdisc_ids(&output)?,
                }
            } else {
                Device {
                    ifindex: None,
                    qdiscs: Vec::new(),
                }
            };
            devices.insert(name.clone(), device);
        }
        let ingress = if paths.sys_class_net.join(&spec.target_interface).exists()
            && spec.download_interface.starts_with("ifb")
        {
            Some(tc_output(
                paths,
                &["filter", "show", "dev", &spec.target_interface, "ingress"],
            )?)
        } else {
            None
        };
        Ok(Self {
            state,
            devices,
            ingress,
        })
    }
}

fn qdisc_ids(bytes: &[u8]) -> Result<Vec<(String, String, String)>, NativeSqmAttestationError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| NativeSqmAttestationError::failed("retained SQM qdisc output invalid"))?;
    let mut ids = BTreeSet::new();
    let handle = |text: &str| {
        let Some((major, minor)) = text.split_once(':') else {
            return false;
        };
        major.len() <= 4
            && minor.len() <= 4
            && (!major.is_empty() || !minor.is_empty())
            && major
                .bytes()
                .chain(minor.bytes())
                .all(|b| b.is_ascii_hexdigit())
    };
    for line in text.lines() {
        let mut fields = line.split_ascii_whitespace();
        if fields.next() != Some("qdisc") {
            continue;
        }
        let kind = fields
            .next()
            .ok_or_else(|| NativeSqmAttestationError::failed("retained SQM qdisc kind missing"))?;
        let id = fields.next().filter(|v| handle(v)).ok_or_else(|| {
            NativeSqmAttestationError::failed("retained SQM qdisc handle invalid")
        })?;
        let parent = match fields.next() {
            Some("root") => "root",
            Some("parent") => fields.next().filter(|v| handle(v)).ok_or_else(|| {
                NativeSqmAttestationError::failed("retained SQM qdisc parent invalid")
            })?,
            _ => {
                return Err(NativeSqmAttestationError::failed(
                    "retained SQM qdisc attachment invalid",
                ))
            }
        };
        if kind.is_empty()
            || !kind.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || !ids.insert((kind.into(), id.into(), parent.into()))
        {
            return Err(NativeSqmAttestationError::failed(
                "retained SQM qdisc identity ambiguous",
            ));
        }
    }
    Ok(ids.into_iter().collect())
}

pub(crate) struct PinnedSqm {
    spec: ManagedSqmAttestationSpec,
    generation: String,
    snapshot: Snapshot,
}
impl PinnedSqm {
    pub(crate) fn capture_under_lifecycle(
        source: &ManagedSqmStopSpec,
        input: &super::super::controller_input::Loaded,
        lease: &super::super::service_lifecycle::ServiceGlobalLease<'_>,
    ) -> Result<Self, NativeSqmAttestationError> {
        Self::capture_in(
            source,
            input,
            &OpenWrtPaths::from_environment(),
            Inspection::Lifecycle(lease),
        )
    }
    pub(crate) fn capture(
        source: &ManagedSqmStopSpec,
        input: &super::super::controller_input::Loaded,
    ) -> Result<Self, NativeSqmAttestationError> {
        Self::capture_with_paths(source, input, &OpenWrtPaths::from_environment())
    }
    pub(super) fn capture_with_paths(
        source: &ManagedSqmStopSpec,
        input: &super::super::controller_input::Loaded,
        paths: &OpenWrtPaths,
    ) -> Result<Self, NativeSqmAttestationError> {
        Self::capture_in(source, input, paths, Inspection::Independent)
    }
    pub(super) fn capture_in(
        source: &ManagedSqmStopSpec,
        input: &super::super::controller_input::Loaded,
        paths: &OpenWrtPaths,
        inspection: Inspection<'_>,
    ) -> Result<Self, NativeSqmAttestationError> {
        source.validate()?;
        input
            .guard
            .attest_applied()
            .map_err(NativeSqmAttestationError::failed)?;
        if input.guard.instance() != source.instance {
            return Err(NativeSqmAttestationError::failed(
                "retained SQM instance mismatch",
            ));
        }
        let sqm = parse_uci_show(
            &input
                .sqm_section(&source.sqm_section)
                .map_err(NativeSqmAttestationError::failed)?,
            "sqm",
            &source.sqm_section,
            "queue",
            UciListPolicy::Reject,
        )?;
        let spec = stop_attestation_spec(source, &sqm)?.ok_or_else(|| {
            NativeSqmAttestationError::failed("retained SQM has no shaping direction")
        })?;
        let _locks = inspection.claim(paths, &spec.target_interface)?;
        validate_input(&spec, input, paths)?;
        let snapshot = Snapshot::read(&spec, paths)?;
        validate_input(&spec, input, paths)?;
        if !snapshot.same(&Snapshot::read(&spec, paths)?) {
            return Err(NativeSqmAttestationError::failed(
                "retained SQM changed during capture",
            ));
        }
        inspection.attest(paths, &spec.target_interface)?;
        Ok(Self {
            spec,
            generation: input.guard.generation().into(),
            snapshot,
        })
    }
    pub(crate) fn attest(
        &self,
        input: &super::super::controller_input::Loaded,
    ) -> Result<(), NativeSqmAttestationError> {
        self.attest_with_paths(input, &OpenWrtPaths::from_environment())
    }
    pub(crate) fn attest_under_lifecycle(
        &self,
        input: &super::super::controller_input::Loaded,
        lease: &super::super::service_lifecycle::ServiceGlobalLease<'_>,
    ) -> Result<(), NativeSqmAttestationError> {
        self.attest_in(
            input,
            &OpenWrtPaths::from_environment(),
            Inspection::Lifecycle(lease),
        )
    }
    pub(super) fn attest_with_paths(
        &self,
        input: &super::super::controller_input::Loaded,
        paths: &OpenWrtPaths,
    ) -> Result<(), NativeSqmAttestationError> {
        self.attest_in(input, paths, Inspection::Independent)
    }
    pub(super) fn attest_in(
        &self,
        input: &super::super::controller_input::Loaded,
        paths: &OpenWrtPaths,
        inspection: Inspection<'_>,
    ) -> Result<(), NativeSqmAttestationError> {
        if input.guard.generation() != self.generation {
            return Err(NativeSqmAttestationError::failed(
                "retained SQM generation changed",
            ));
        }
        let _locks = inspection.claim(paths, &self.spec.target_interface)?;
        validate_input(&self.spec, input, paths)?;
        if !self.snapshot.same(&Snapshot::read(&self.spec, paths)?) {
            return Err(NativeSqmAttestationError::failed(
                "retained SQM runtime identity changed",
            ));
        }
        validate_input(&self.spec, input, paths)?;
        inspection.attest(paths, &self.spec.target_interface)
    }
}

fn validate_input(
    spec: &ManagedSqmAttestationSpec,
    input: &super::super::controller_input::Loaded,
    paths: &OpenWrtPaths,
) -> Result<(), NativeSqmAttestationError> {
    input
        .guard
        .attest_applied()
        .map_err(NativeSqmAttestationError::failed)?;
    let sqm = input_sqm_sections(spec, input)?;
    if paths.sys_class_net.join(&spec.target_interface).exists() {
        let state = load_runtime_identity(spec, paths)?;
        validate_state_against_sqm(spec, &state, &sqm)?;
        validate_kernel_topology(spec, &state, paths)?;
    } else if let Some(state) = load_optional_runtime_identity(spec, paths)? {
        validate_state_against_sqm(spec, &state, &sqm)?;
        validate_offline_target_topology(spec, &state, paths)?;
    } else {
        attest_managed_sqm_kernel_absent(&stop_spec_from_attestation(spec), paths)?;
    }
    input
        .guard
        .attest_applied()
        .map_err(NativeSqmAttestationError::failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn r4_pinned_qdisc_identity_ignores_rates_and_order_but_detects_handle_attachment_changes() {
        let first = b"qdisc cake 8001: root refcnt 2 bandwidth 20Mbit\nqdisc ingress ffff: parent ffff:fff1\n";
        let changed_rate = b"qdisc ingress ffff: parent ffff:fff1\nqdisc cake 8001: root refcnt 3 bandwidth 30Mbit\n";
        assert_eq!(qdisc_ids(first).unwrap(), qdisc_ids(changed_rate).unwrap());
        for changed in [
            b"qdisc cake 8002: root bandwidth 20Mbit\nqdisc ingress ffff: parent ffff:fff1\n"
                .as_slice(),
            b"qdisc cake_mq 8001: root bandwidth 20Mbit\nqdisc ingress ffff: parent ffff:fff1\n"
                .as_slice(),
            b"qdisc cake 8001: parent 1:1 bandwidth 20Mbit\nqdisc ingress ffff: parent ffff:fff1\n"
                .as_slice(),
        ] {
            assert_ne!(qdisc_ids(first).unwrap(), qdisc_ids(changed).unwrap());
        }
        for invalid in [
            b"qdisc cake invalid root".as_slice(),
            b"qdisc cake 1:1 wrong".as_slice(),
            b"qdisc cake 1:1 parent invalid".as_slice(),
            b"qdisc cake 1: root\nqdisc cake 1: root\n".as_slice(),
        ] {
            assert!(qdisc_ids(invalid).is_err());
        }
    }
}
