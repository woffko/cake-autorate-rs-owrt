//! Operation deadlines on the monotonic boot clock.
//!
//! A request keeps its wall-clock `created_unix_ms` and `deadline_unix_ms`, but
//! the wall clock can be stepped at any time: an NTP correction, a manual
//! `date -s`, or a router that boots with a stale time and synchronises later.
//! A step must neither expire a running operation early nor extend it.
//!
//! When the coordinator admits a job it therefore anchors the operation's
//! duration (`deadline_unix_ms - created_unix_ms`) on the kernel boot clock and
//! stores the result in the job directory beside the request. Every later
//! decision (coordinator, workers, runtime owner) compares the boot clock with
//! that anchor. The request format does not change.
//!
//! The anchor belongs to one boot. After a reboot the boot ID differs, the
//! anchor is refused and the journal scan already treats the job as
//! interrupted.

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::identity::{monotonic_boot_ms, read_kernel_uuid, DEFAULT_BOOT_ID_PATH};
use super::protocol::OperationRequest;
use super::rating::{epoch_ms, read_private_bounded};

pub const DEADLINE_FILE: &str = "deadline";
const DEADLINE_HEADER: &str = "cake-autorate-operation-deadline\t1";
const MAX_DEADLINE_BYTES: usize = 256;

/// A point on the kernel boot clock, in milliseconds since boot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OperationDeadline {
    boot_ms: u64,
}

impl OperationDeadline {
    pub const fn at_boot_ms(boot_ms: u64) -> Self {
        Self { boot_ms }
    }

    #[cfg(test)]
    pub const fn boot_ms(self) -> u64 {
        self.boot_ms
    }

    pub const fn remaining_ms_at(self, now_boot_ms: u64) -> u64 {
        self.boot_ms.saturating_sub(now_boot_ms)
    }

    pub const fn expired_at(self, now_boot_ms: u64) -> bool {
        now_boot_ms >= self.boot_ms
    }

    pub fn remaining_ms(self) -> Result<u64, String> {
        Ok(self.remaining_ms_at(monotonic_boot_ms()?))
    }

    pub fn expired(self) -> Result<bool, String> {
        Ok(self.expired_at(monotonic_boot_ms()?))
    }

    /// The earlier of this deadline and a sub-deadline that is
    /// `duration_ms` away from now.
    pub fn within_ms(self, duration_ms: u64) -> Result<Self, String> {
        let local = monotonic_boot_ms()?
            .checked_add(duration_ms)
            .ok_or_else(|| "operation sub-deadline overflows".to_string())?;
        Ok(Self::at_boot_ms(self.boot_ms.min(local)))
    }

    /// The wall-clock instant at which this deadline falls if the clocks keep
    /// their present relation. Only for formats that carry a wall-clock
    /// deadline and cannot change (the rating capture request); the value is
    /// right at the moment of publication, not later.
    pub fn wall_clock_unix_ms(self) -> Result<u64, String> {
        epoch_ms()?
            .checked_add(self.remaining_ms()?)
            .ok_or_else(|| "operation deadline overflows the wall clock".to_string())
    }
}

/// The persisted form of a deadline: the boot it belongs to and its place on
/// that boot's clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeadlineAnchor {
    pub boot_id: String,
    pub deadline_boot_ms: u64,
}

impl DeadlineAnchor {
    /// Anchor the request's duration at the moment of admission.
    pub fn admit(
        request: &OperationRequest,
        boot_id: &str,
        now_boot_ms: u64,
    ) -> Result<Self, String> {
        let duration_ms = request
            .deadline_unix_ms
            .checked_sub(request.created_unix_ms)
            .ok_or_else(|| "operation deadline precedes its creation".to_string())?;
        Self::after(boot_id, now_boot_ms, duration_ms)
    }

    fn after(boot_id: &str, now_boot_ms: u64, duration_ms: u64) -> Result<Self, String> {
        let deadline_boot_ms = now_boot_ms
            .checked_add(duration_ms)
            .ok_or_else(|| "operation deadline overflows the boot clock".to_string())?;
        Ok(Self {
            boot_id: boot_id.to_string(),
            deadline_boot_ms,
        })
    }

    pub fn encode(&self) -> String {
        format!(
            "{DEADLINE_HEADER}\nboot_id={}\ndeadline_boot_ms={}\n",
            self.boot_id, self.deadline_boot_ms
        )
    }

    pub fn decode(input: &str) -> Result<Self, String> {
        let mut lines = input.lines();
        if lines.next() != Some(DEADLINE_HEADER) || !input.ends_with('\n') {
            return Err("operation deadline record has an unsupported header".to_string());
        }
        let boot_id = lines
            .next()
            .and_then(|line| line.strip_prefix("boot_id="))
            .filter(|value| {
                value.len() == 32
                    && value
                        .bytes()
                        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
            })
            .ok_or_else(|| "operation deadline record has no valid boot ID".to_string())?;
        let deadline_boot_ms = lines
            .next()
            .and_then(|line| line.strip_prefix("deadline_boot_ms="))
            .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| "operation deadline record has no valid deadline".to_string())?;
        if lines.next().is_some() {
            return Err("operation deadline record has trailing data".to_string());
        }
        Ok(Self {
            boot_id: boot_id.to_string(),
            deadline_boot_ms,
        })
    }

    /// The deadline, if the anchor was written during the boot named
    /// `boot_id`.
    pub fn on_boot(&self, boot_id: &str) -> Result<OperationDeadline, String> {
        if self.boot_id != boot_id {
            return Err("operation-deadline-from-another-boot".to_string());
        }
        Ok(OperationDeadline::at_boot_ms(self.deadline_boot_ms))
    }
}

/// Create the anchor file of a newly admitted job. The job directory is new
/// and private; the file is published atomically with owner-only access.
pub fn write_anchor(job_directory: &Path, anchor: &DeadlineAnchor) -> Result<(), String> {
    let contents = anchor.encode();
    let temporary = job_directory.join(format!(".{DEADLINE_FILE}.tmp"));
    // Only the coordinator writes anchors, one at a time: a temporary file
    // that is still here was left by a crash and is safe to replace.
    let _ = fs::remove_file(&temporary);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("unable to create the operation deadline: {error}"))?;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("unable to secure the operation deadline: {error}"))?;
        file.write_all(contents.as_bytes())
            .map_err(|error| format!("unable to write the operation deadline: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("unable to sync the operation deadline: {error}"))?;
        fs::rename(&temporary, job_directory.join(DEADLINE_FILE))
            .map_err(|error| format!("unable to publish the operation deadline: {error}"))?;
        File::open(job_directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("unable to sync the job directory: {error}"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// The deadline of the job stored in `job_directory`, for the boot `boot_id`.
///
/// A job admitted before anchors existed has no file. Its remaining wall-clock
/// time is converted once; `persist` stores the result so that every process
/// then agrees on it (only the coordinator persists).
pub fn load_on_boot(
    job_directory: &Path,
    request: &OperationRequest,
    boot_id: &str,
    persist: bool,
) -> Result<OperationDeadline, String> {
    let path = job_directory.join(DEADLINE_FILE);
    match fs::symlink_metadata(&path) {
        Ok(_) => DeadlineAnchor::decode(&read_private_bounded(&path, MAX_DEADLINE_BYTES)?)?
            .on_boot(boot_id),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let remaining_ms = request.deadline_unix_ms.saturating_sub(epoch_ms()?);
            let anchor = DeadlineAnchor::after(boot_id, monotonic_boot_ms()?, remaining_ms)?;
            if persist {
                write_anchor(job_directory, &anchor)?;
            }
            Ok(OperationDeadline::at_boot_ms(anchor.deadline_boot_ms))
        }
        Err(error) => Err(format!("unable to inspect the operation deadline: {error}")),
    }
}

/// The deadline stored in `job_directory` for the boot `boot_id`, or `None`
/// when the job has no anchor file. Reads only; never creates the anchor.
pub fn load_existing(
    job_directory: &Path,
    boot_id: &str,
) -> Result<Option<OperationDeadline>, String> {
    let path = job_directory.join(DEADLINE_FILE);
    match fs::symlink_metadata(&path) {
        Ok(_) => DeadlineAnchor::decode(&read_private_bounded(&path, MAX_DEADLINE_BYTES)?)?
            .on_boot(boot_id)
            .map(Some),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("unable to inspect the operation deadline: {error}")),
    }
}

/// The deadline for a worker process that was started with `--request`.
pub fn load_for_worker(
    request_path: &Path,
    request: &OperationRequest,
) -> Result<OperationDeadline, String> {
    let job_directory = request_path
        .parent()
        .ok_or_else(|| "operation request has no job directory".to_string())?;
    let boot_id = read_kernel_uuid(Path::new(DEFAULT_BOOT_ID_PATH), "boot ID")?;
    load_on_boot(job_directory, request, &boot_id, false)
}

#[cfg(test)]
mod tests {
    use super::super::identity::test_clock::ScriptedClocks;
    use super::super::protocol::{
        CalibrationStrategy, OperationIdentity, OperationKind, OperationOrigin,
        OperationRouteIdentity, OperationRouteMode, OperationTargetState, TrafficPolicy,
    };
    use super::*;
    use crate::autotune::{
        AccessEvidenceSource, AccessMedium, AutotuneProfile, CapacityLearningPolicy,
    };
    use std::path::PathBuf;

    const BOOT_ID: &str = "11111111111111111111111111111111";
    const OTHER_BOOT_ID: &str = "22222222222222222222222222222222";
    const FORTY_FIVE_MINUTES_MS: u64 = 45 * 60 * 1_000;

    fn request(created_unix_ms: u64, duration_ms: u64) -> OperationRequest {
        OperationRequest {
            identity: OperationIdentity {
                job_id: "33333333333333333333333333333333".to_string(),
                job_token: "4".repeat(64),
                instance: "wan_sqm".to_string(),
                operation: OperationKind::FullAutotune,
                target_interface: "pppoe-wan".to_string(),
                route_fingerprint: "5".repeat(64),
                config_fingerprint: "6".repeat(64),
                sqm_fingerprint: "7".repeat(64),
            },
            created_unix_ms,
            deadline_unix_ms: created_unix_ms + duration_ms,
            origin: OperationOrigin::Luci,
            backend: "speedtest-go".to_string(),
            speedtest_direction: None,
            speedtest_server_id: None,
            speedtest_topology: None,
            route: OperationRouteIdentity {
                dns_server: None,
                device_ifindex: None,
                mode: OperationRouteMode::Main,
                mwan3_member: None,
                l3_device: "pppoe-wan".to_string(),
                source_ip: None,
                fwmark: None,
                routing_table: None,
                fwmark_mask: None,
            },
            target_state: OperationTargetState::ExistingManaged,
            capture_policy: None,
            managed_sqm_section: Some("wan_sqm".to_string()),
            profile: Some(AutotuneProfile::VariableLink),
            strategy: Some(CalibrationStrategy::FullRaw),
            access_medium: Some(AccessMedium::Cellular),
            access_source: Some(AccessEvidenceSource::UserSelected),
            access_confidence_percent: 100,
            capacity_learning_policy: Some(CapacityLearningPolicy::PassiveBounded),
            service_dl_cap_kbps: None,
            service_ul_cap_kbps: None,
            allow_sqm_disable: true,
            allow_active_traffic: true,
            scheduled_auto_apply_requested: false,
            traffic_budget: TrafficPolicy::Capped {
                max_bytes: 1_000_000,
            },
            traffic_policy_explicit: false,
            traffic_plan: None,
            server_failure_retries: None,
        }
    }

    fn job_directory(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("cake-deadline-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    #[test]
    fn a_deadline_is_inclusive_and_never_underflows() {
        let deadline = OperationDeadline::at_boot_ms(1_000);
        assert!(!deadline.expired_at(999));
        assert!(deadline.expired_at(1_000));
        assert!(deadline.expired_at(u64::MAX));
        assert_eq!(deadline.remaining_ms_at(400), 600);
        assert_eq!(deadline.remaining_ms_at(1_000), 0);
        assert_eq!(deadline.remaining_ms_at(5_000), 0);
    }

    #[test]
    fn admission_anchors_the_duration_whatever_the_wall_clock_says() {
        // The same 45 minutes, requested at the Unix epoch, in the present
        // and in 2100: the anchor depends only on the duration and the boot
        // clock at admission.
        for created in [1, 1_790_000_000_000, 4_102_444_800_000] {
            let anchor =
                DeadlineAnchor::admit(&request(created, FORTY_FIVE_MINUTES_MS), BOOT_ID, 90_000)
                    .unwrap();
            assert_eq!(anchor.deadline_boot_ms, 90_000 + FORTY_FIVE_MINUTES_MS);
            assert_eq!(anchor.boot_id, BOOT_ID);
        }
        let mut backwards = request(5_000, 0);
        backwards.deadline_unix_ms = 4_999;
        assert!(DeadlineAnchor::admit(&backwards, BOOT_ID, 0).is_err());
        assert!(DeadlineAnchor::admit(&request(1, u64::MAX - 1), BOOT_ID, 2).is_err());
    }

    #[test]
    fn the_anchor_record_is_strict() {
        let anchor = DeadlineAnchor {
            boot_id: BOOT_ID.to_string(),
            deadline_boot_ms: 123_456,
        };
        let text = anchor.encode();
        assert_eq!(
            text,
            "cake-autorate-operation-deadline\t1\nboot_id=11111111111111111111111111111111\ndeadline_boot_ms=123456\n"
        );
        assert_eq!(DeadlineAnchor::decode(&text).unwrap(), anchor);
        for broken in [
            text.replace("\t1", "\t2"),
            text.replace("boot_id=1111", "boot_id=GGGG"),
            text.replace("deadline_boot_ms=123456", "deadline_boot_ms=-1"),
            text.replace("deadline_boot_ms=123456", "deadline_boot_ms="),
            text.replace("deadline_boot_ms=123456", "deadline_boot_ms=1x"),
            format!("{text}extra=1\n"),
            text.trim_end().to_string(),
            String::new(),
        ] {
            assert!(DeadlineAnchor::decode(&broken).is_err(), "{broken:?}");
        }
    }

    #[test]
    fn an_anchor_belongs_to_one_boot() {
        let anchor = DeadlineAnchor {
            boot_id: BOOT_ID.to_string(),
            deadline_boot_ms: 7,
        };
        assert_eq!(
            anchor.on_boot(BOOT_ID).unwrap(),
            OperationDeadline::at_boot_ms(7)
        );
        assert_eq!(
            anchor.on_boot(OTHER_BOOT_ID).unwrap_err(),
            "operation-deadline-from-another-boot"
        );
    }

    #[test]
    fn the_anchor_file_is_private_and_read_back_exactly() {
        let directory = job_directory("roundtrip");
        let anchor = DeadlineAnchor {
            boot_id: BOOT_ID.to_string(),
            deadline_boot_ms: 98_765,
        };
        write_anchor(&directory, &anchor).unwrap();
        let path = directory.join(DEADLINE_FILE);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(!directory.join(format!(".{DEADLINE_FILE}.tmp")).exists());
        let request = request(1, FORTY_FIVE_MINUTES_MS);
        assert_eq!(
            load_on_boot(&directory, &request, BOOT_ID, false).unwrap(),
            OperationDeadline::at_boot_ms(98_765)
        );
        assert!(load_on_boot(&directory, &request, OTHER_BOOT_ID, false).is_err());
        // A second publication must not reuse a stale temporary file.
        write_anchor(&directory, &anchor).unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_worker_reads_the_anchor_beside_its_request() {
        let directory = job_directory("worker");
        let boot_id = read_kernel_uuid(Path::new(DEFAULT_BOOT_ID_PATH), "boot ID").unwrap();
        let request = request(1, FORTY_FIVE_MINUTES_MS);
        let request_path = directory.join("request");
        write_anchor(
            &directory,
            &DeadlineAnchor {
                boot_id: boot_id.clone(),
                deadline_boot_ms: 4_242,
            },
        )
        .unwrap();
        assert_eq!(
            load_for_worker(&request_path, &request).unwrap(),
            OperationDeadline::at_boot_ms(4_242)
        );
        // An anchor written during another boot is refused.
        let foreign = if boot_id == OTHER_BOOT_ID {
            BOOT_ID
        } else {
            OTHER_BOOT_ID
        };
        write_anchor(
            &directory,
            &DeadlineAnchor {
                boot_id: foreign.to_string(),
                deadline_boot_ms: 4_242,
            },
        )
        .unwrap();
        assert_eq!(
            load_for_worker(&request_path, &request).unwrap_err(),
            "operation-deadline-from-another-boot"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_job_without_an_anchor_gets_one_from_its_remaining_time_once() {
        let clocks = ScriptedClocks::install(500_000, 1_790_000_000_000);
        let directory = job_directory("legacy");
        // Ten minutes of the wall-clock deadline remain at the first lookup.
        let request = request(1_790_000_000_000 - 35 * 60_000, FORTY_FIVE_MINUTES_MS);
        let first = load_on_boot(&directory, &request, BOOT_ID, true).unwrap();
        assert_eq!(first.boot_ms(), 500_000 + 10 * 60_000);
        assert!(directory.join(DEADLINE_FILE).exists());

        // From then on the stored anchor rules, whatever the wall clock does.
        clocks.advance(60_000);
        clocks.step_wall_forward(3 * 3_600_000);
        assert_eq!(
            load_on_boot(&directory, &request, BOOT_ID, true).unwrap(),
            first
        );
        clocks.step_wall_back(8 * 3_600_000);
        assert_eq!(
            load_on_boot(&directory, &request, BOOT_ID, true).unwrap(),
            first
        );

        // A worker that finds no anchor derives one but does not publish it.
        let worker_directory = job_directory("legacy-worker");
        let derived = load_on_boot(&worker_directory, &request, BOOT_ID, false).unwrap();
        assert!(derived.boot_ms() > 0);
        assert!(!worker_directory.join(DEADLINE_FILE).exists());
        fs::remove_dir_all(directory).unwrap();
        fs::remove_dir_all(worker_directory).unwrap();
    }

    #[test]
    fn clock_steps_neither_expire_nor_extend_an_operation() {
        let clocks = ScriptedClocks::install(1_000_000, 1_790_000_000_000);
        let anchor = DeadlineAnchor::admit(
            &request(1_790_000_000_000, 10 * 60_000),
            BOOT_ID,
            clocks.boot(),
        )
        .unwrap();
        let deadline = anchor.on_boot(BOOT_ID).unwrap();
        assert!(!deadline.expired().unwrap());
        assert_eq!(deadline.remaining_ms().unwrap(), 10 * 60_000);

        clocks.advance(5 * 60_000);
        assert_eq!(deadline.remaining_ms().unwrap(), 5 * 60_000);

        // Forward by an hour: the old wall-clock comparison would have
        // expired the operation here.
        clocks.step_wall_forward(3_600_000);
        assert!(!deadline.expired().unwrap());
        assert_eq!(deadline.remaining_ms().unwrap(), 5 * 60_000);

        // Back by two hours: the old comparison would have granted another
        // two hours.
        clocks.step_wall_back(2 * 3_600_000);
        assert_eq!(deadline.remaining_ms().unwrap(), 5 * 60_000);
        clocks.advance(5 * 60_000 - 1);
        assert!(!deadline.expired().unwrap());
        clocks.advance(1);
        assert!(deadline.expired().unwrap());
        assert_eq!(deadline.remaining_ms().unwrap(), 0);
    }

    #[test]
    fn a_sub_deadline_never_extends_the_operation() {
        let clocks = ScriptedClocks::install(10_000, 1_790_000_000_000);
        let deadline = OperationDeadline::at_boot_ms(70_000);
        assert_eq!(deadline.within_ms(30_000).unwrap().boot_ms(), 40_000);
        assert_eq!(deadline.within_ms(90_000).unwrap().boot_ms(), 70_000);
        clocks.advance(45_000);
        assert_eq!(deadline.within_ms(30_000).unwrap().boot_ms(), 70_000);
        assert!(OperationDeadline::at_boot_ms(u64::MAX)
            .within_ms(u64::MAX)
            .is_err());
    }

    #[test]
    fn formats_that_carry_a_wall_clock_deadline_follow_the_remaining_time() {
        let clocks = ScriptedClocks::install(100_000, 1_790_000_000_000);
        let deadline = OperationDeadline::at_boot_ms(160_000);
        assert_eq!(deadline.wall_clock_unix_ms().unwrap(), 1_790_000_060_000);
        // After a clock step the published instant is derived again from the
        // remaining boot time, so it stays one minute away.
        clocks.step_wall_forward(7_200_000);
        assert_eq!(deadline.wall_clock_unix_ms().unwrap(), 1_790_007_260_000);
        clocks.step_wall_back(14_400_000);
        assert_eq!(deadline.wall_clock_unix_ms().unwrap(), 1_789_992_860_000);
    }
}
