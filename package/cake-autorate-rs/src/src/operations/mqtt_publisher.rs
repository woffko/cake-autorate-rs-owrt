//! Native Home Assistant MQTT telemetry publisher.
//!
//! The legacy implementation reconstructed controller state by joining a
//! `tail | awk | mosquitto_pub` pipeline through predictable FIFOs. This
//! module keeps the publisher as a non-authoritative procd sidecar, but owns
//! configuration, aggregation and the MQTT wire contract in Rust. The live
//! reader is event-driven; loss of the broker is terminal so procd, rather
//! than an internal sleep loop, owns bounded restart policy.

use super::json_wire::json_escape;
use super::runtime_health::{safe_name, UciPackage, UciSection};
use ring::digest::{digest, SHA256};
use std::collections::BTreeSet;
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const MAX_INSTANCES: usize = 64;
const MAX_TEXT_BYTES: usize = 4096;
const MAX_TOPIC_BYTES: usize = 512;
const MAX_PLAN_BYTES: usize = 32 * 1024;
const MAX_MQTT_PACKET_BYTES: usize = 128 * 1024;
const MAX_CPU_CORES: usize = 256;
const MQTT_KEEPALIVE_SECONDS: u16 = 60;
const MQTT_IO_TIMEOUT: Duration = Duration::from_secs(5);
const MQTT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RESOLVED_ADDRESSES: usize = 8;
const MAX_LOG_READ_BYTES: usize = 1024 * 1024;
const MAX_PARTIAL_LINE_BYTES: usize = 64 * 1024;
const PLAN_MAGIC: &[u8] = b"cake-autorate-mqtt-plan\0\x01";
const PRODUCTION_PLAN_ROOT: &str = "/var/run/cake-autorate-mqtt";
static PLAN_SEQUENCE: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MqttPublisherConfig {
    pub(crate) instance: String,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) username: String,
    pub(crate) password: String,
    pub(crate) discovery_prefix: String,
    pub(crate) base_topic: String,
    pub(crate) device_id: String,
    pub(crate) device_name: String,
    pub(crate) min_interval: Duration,
    pub(crate) publish_cpu: bool,
    pub(crate) log_directory: String,
}

impl MqttPublisherConfig {
    pub(crate) fn from_section(
        instance: &str,
        section: &UciSection,
    ) -> Result<Option<Self>, String> {
        if section.section_type != "cake_autorate" {
            return Err(format!(
                "MQTT instance {instance} has an unexpected section type"
            ));
        }
        if !bool_option(section, "mqtt_enabled", false)? {
            return Ok(None);
        }
        if !bool_option(section, "enabled", false)? {
            return Err(format!(
                "MQTT instance {instance} requires the controller instance to be enabled"
            ));
        }
        if !bool_option(section, "log_to_file", true)? {
            return Err(format!(
                "MQTT instance {instance} requires log_to_file=1 until direct telemetry transport is enabled"
            ));
        }
        if !bool_option(section, "output_summary_stats", false)? {
            return Err(format!(
                "MQTT instance {instance} requires output_summary_stats=1"
            ));
        }
        let publish_cpu = bool_option(section, "mqtt_publish_cpu_stats", false)?;
        if publish_cpu && !bool_option(section, "output_cpu_stats", false)? {
            return Err(format!(
                "MQTT instance {instance} requires output_cpu_stats=1 when CPU sensors are enabled"
            ));
        }

        let host = required_text(section, "mqtt_host", 255)?;
        let port = option(section, "mqtt_port")
            .unwrap_or("1883")
            .parse::<u16>()
            .map_err(|_| format!("MQTT instance {instance} has an invalid mqtt_port"))?;
        if port == 0 {
            return Err(format!("MQTT instance {instance} has an invalid mqtt_port"));
        }
        let interval = option(section, "mqtt_min_interval_s")
            .unwrap_or("1")
            .parse::<u64>()
            .map_err(|_| format!("MQTT instance {instance} has an invalid mqtt_min_interval_s"))?;
        if !(1..=3600).contains(&interval) {
            return Err(format!(
                "MQTT instance {instance} mqtt_min_interval_s must be between 1 and 3600"
            ));
        }

        let username = optional_text(section, "mqtt_username", MAX_TEXT_BYTES)?;
        let password = optional_text(section, "mqtt_password", MAX_TEXT_BYTES)?;
        let discovery_prefix = topic_value(section, "mqtt_discovery_prefix", "homeassistant")?;
        let base_topic = topic_value(section, "mqtt_base_topic", "cake-autorate")?;
        let device_prefix =
            safe_identifier(option(section, "mqtt_device_id").unwrap_or("cake_autorate"));
        let device_name_prefix = text_value(section, "mqtt_device_name", "cake-autorate", 256)?;
        let log_directory = text_value(section, "log_file_path_override", "/var/log", 1024)?;
        if !log_directory.starts_with('/') {
            return Err(format!(
                "MQTT instance {instance} log_file_path_override must be absolute"
            ));
        }
        let safe_instance = safe_identifier(instance);
        let config = Self {
            instance: instance.to_string(),
            host,
            port,
            username,
            password,
            discovery_prefix,
            base_topic,
            device_id: format!("{device_prefix}_{safe_instance}"),
            device_name: format!("{device_name_prefix} ({instance})"),
            min_interval: Duration::from_secs(interval),
            publish_cpu,
            log_directory,
        };
        config.validate_plan()?;
        Ok(Some(config))
    }

    pub(crate) fn state_topic(&self) -> String {
        format!("{}/{}", self.base_topic, self.instance)
    }

    pub(crate) fn availability_topic(&self) -> String {
        format!("{}/availability", self.state_topic())
    }

    pub(crate) fn log_path(&self) -> std::path::PathBuf {
        std::path::Path::new(&self.log_directory)
            .join(format!("cake-autorate.{}.log", self.instance))
    }

    fn validate_plan(&self) -> Result<(), String> {
        if !safe_name(&self.instance) || self.instance.len() > 64 {
            return Err("MQTT plan contains an unsafe instance".to_string());
        }
        validate_text(&self.host, 255)?;
        if self.host.is_empty() || self.port == 0 {
            return Err("MQTT plan contains an invalid broker".to_string());
        }
        validate_text(&self.username, MAX_TEXT_BYTES)?;
        validate_text(&self.password, MAX_TEXT_BYTES)?;
        if !self.password.is_empty() && self.username.is_empty() {
            return Err("MQTT password requires a username under MQTT 3.1.1".to_string());
        }
        validate_topic(&self.discovery_prefix)?;
        validate_topic(&self.base_topic)?;
        validate_text(&self.device_id, 256)?;
        validate_text(&self.device_name, 256)?;
        if self.device_id.is_empty()
            || !self
                .device_id
                .chars()
                .all(|value| value.is_ascii_alphanumeric() || value == '_')
        {
            return Err("MQTT plan contains an unsafe device identifier".to_string());
        }
        if !(1..=3600).contains(&self.min_interval.as_secs())
            || self.min_interval.subsec_nanos() != 0
        {
            return Err("MQTT plan contains an invalid publish interval".to_string());
        }
        validate_text(&self.log_directory, 1024)?;
        if !self.log_directory.starts_with('/') {
            return Err("MQTT plan contains a non-absolute log directory".to_string());
        }
        if self.state_topic().len() > MAX_TOPIC_BYTES
            || self.availability_topic().len() > MAX_TOPIC_BYTES
        {
            return Err("MQTT plan produces an oversized topic".to_string());
        }
        Ok(())
    }

    fn encode_plan(&self) -> Result<Vec<u8>, String> {
        self.validate_plan()?;
        let mut bytes = Vec::with_capacity(1024);
        bytes.extend_from_slice(PLAN_MAGIC);
        for value in [
            self.instance.as_str(),
            self.host.as_str(),
            self.username.as_str(),
            self.password.as_str(),
            self.discovery_prefix.as_str(),
            self.base_topic.as_str(),
            self.device_id.as_str(),
            self.device_name.as_str(),
            self.log_directory.as_str(),
        ] {
            push_plan_text(&mut bytes, value)?;
        }
        bytes.extend_from_slice(&self.port.to_be_bytes());
        bytes.extend_from_slice(&self.min_interval.as_secs().to_be_bytes());
        bytes.push(u8::from(self.publish_cpu));
        if bytes.len() > MAX_PLAN_BYTES {
            return Err("MQTT plan exceeds its byte bound".to_string());
        }
        Ok(bytes)
    }

    fn decode_plan(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_PLAN_BYTES || !bytes.starts_with(PLAN_MAGIC) {
            return Err("MQTT plan header is invalid".to_string());
        }
        let mut reader = PlanReader::new(&bytes[PLAN_MAGIC.len()..]);
        let instance = reader.text()?;
        let host = reader.text()?;
        let username = reader.text()?;
        let password = reader.text()?;
        let discovery_prefix = reader.text()?;
        let base_topic = reader.text()?;
        let device_id = reader.text()?;
        let device_name = reader.text()?;
        let log_directory = reader.text()?;
        let port = reader.u16()?;
        let interval = reader.u64()?;
        let publish_cpu = match reader.byte()? {
            0 => false,
            1 => true,
            _ => return Err("MQTT plan contains an invalid boolean".to_string()),
        };
        if !reader.finished() {
            return Err("MQTT plan contains trailing data".to_string());
        }
        let config = Self {
            instance,
            host,
            port,
            username,
            password,
            discovery_prefix,
            base_topic,
            device_id,
            device_name,
            min_interval: Duration::from_secs(interval),
            publish_cpu,
            log_directory,
        };
        config.validate_plan()?;
        if config.encode_plan()? != bytes {
            return Err("MQTT plan is not canonical".to_string());
        }
        Ok(config)
    }
}

pub(crate) fn run_mqtt_publisher<I>(mut arguments: I) -> Result<(), String>
where
    I: Iterator<Item = String>,
{
    let instance = arguments
        .next()
        .ok_or_else(|| "mqtt-publisher requires an instance".to_string())?;
    if arguments.next().is_some() {
        return Err("mqtt-publisher accepts exactly one instance".to_string());
    }
    let config = read_service_plan(Path::new(PRODUCTION_PLAN_ROOT), &instance)?;
    let signals = SignalEvents::new()?;
    let mut follower = LogFollower::open(config.log_path())?;
    let mut changes = LogEvents::new(follower.path())?;
    let keepalive =
        MonotonicTimer::periodic(Duration::from_secs(u64::from(MQTT_KEEPALIVE_SECONDS) / 2))?;
    let cpu_cores = cpu_core_count()?;
    let mut client = MqttClient::connect(&config)?;
    for message in discovery_messages(&config, cpu_cores)? {
        client.publish(&message.topic, message.payload.as_bytes(), message.retain)?;
    }
    client.publish(&config.availability_topic(), b"online", true)?;

    let started = Instant::now();
    let mut aggregate = MqttAggregator::new(config.min_interval, config.publish_cpu);
    loop {
        let mut descriptors = [
            libc::pollfd {
                fd: signals.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: changes.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: keepalive.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: client.as_raw_fd(),
                events: libc::POLLIN | libc::POLLERR | libc::POLLHUP,
                revents: 0,
            },
        ];
        let result = unsafe {
            libc::poll(
                descriptors.as_mut_ptr(),
                descriptors.len() as libc::nfds_t,
                -1,
            )
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("MQTT event poll failed: {error}"));
        }
        if descriptors[0].revents & libc::POLLIN != 0 {
            signals.drain()?;
            let _ = client.publish(&config.availability_topic(), b"offline", true);
            let _ = client.disconnect();
            return Ok(());
        }
        if descriptors[3].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err("MQTT broker connection closed".to_string());
        }
        if descriptors[3].revents & libc::POLLIN != 0 {
            client.read_unsolicited()?;
        }
        if descriptors[2].revents & libc::POLLIN != 0 {
            keepalive.drain()?;
            client.ping()?;
        }
        if descriptors[1].revents & libc::POLLIN != 0 {
            changes.drain()?;
            let (lines, reopened) = follower.read_available()?;
            if reopened {
                changes.watch_file(follower.path())?;
            }
            for line in lines {
                match aggregate.observe_line(&line, started.elapsed()) {
                    Ok(Some(payload)) => {
                        client.publish(&config.state_topic(), payload.as_bytes(), false)?
                    }
                    Ok(None) => {}
                    Err(error) => eprintln!("WARNING: ignored invalid MQTT source record: {error}"),
                }
            }
        }
    }
}

pub(crate) fn publish_service_plans(
    package: &UciPackage,
    root: &Path,
) -> Result<Vec<String>, String> {
    let mut configs = Vec::new();
    for (name, section) in &package.sections {
        if section.section_type != "cake_autorate" {
            continue;
        }
        if !bool_option(section, "mqtt_enabled", false)? {
            continue;
        }
        let config = MqttPublisherConfig::from_section(name, section)?
            .ok_or_else(|| format!("MQTT instance {name} is not enabled for publishing"))?;
        if configs.len() >= MAX_INSTANCES {
            return Err("too many MQTT publisher instances".to_string());
        }
        configs.push(config);
    }
    let plans = configs
        .iter()
        .map(|config| {
            config
                .encode_plan()
                .map(|bytes| (config.instance.clone(), bytes))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if plans.is_empty() {
        cleanup_plan_root(root)?;
        return Ok(Vec::new());
    }
    publish_plan_files(root, &plans)?;
    Ok(configs.into_iter().map(|config| config.instance).collect())
}

pub(crate) fn publish_production_service_plans(
    package: &UciPackage,
) -> Result<Vec<String>, String> {
    publish_service_plans(package, Path::new(PRODUCTION_PLAN_ROOT))
}

pub(crate) fn cleanup_production_service_plans() -> Result<(), String> {
    cleanup_plan_root(Path::new(PRODUCTION_PLAN_ROOT))
}

fn push_plan_text(bytes: &mut Vec<u8>, value: &str) -> Result<(), String> {
    let length = u32::try_from(value.len()).map_err(|_| "MQTT plan field is too large")?;
    let next = bytes
        .len()
        .checked_add(4)
        .and_then(|size| size.checked_add(value.len()))
        .ok_or_else(|| "MQTT plan size overflow".to_string())?;
    if next > MAX_PLAN_BYTES {
        return Err("MQTT plan exceeds its byte bound".to_string());
    }
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(value.as_bytes());
    Ok(())
}

struct PlanReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> PlanReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], String> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| "MQTT plan offset overflow".to_string())?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| "MQTT plan is truncated".to_string())?;
        self.offset = end;
        Ok(value)
    }

    fn text(&mut self) -> Result<String, String> {
        let mut length = [0u8; 4];
        length.copy_from_slice(self.take(4)?);
        let length = usize::try_from(u32::from_be_bytes(length))
            .map_err(|_| "MQTT plan field length is invalid".to_string())?;
        let value = self.take(length)?;
        std::str::from_utf8(value)
            .map(str::to_string)
            .map_err(|_| "MQTT plan field is not UTF-8".to_string())
    }

    fn u16(&mut self) -> Result<u16, String> {
        let mut value = [0u8; 2];
        value.copy_from_slice(self.take(2)?);
        Ok(u16::from_be_bytes(value))
    }

    fn u64(&mut self) -> Result<u64, String> {
        let mut value = [0u8; 8];
        value.copy_from_slice(self.take(8)?);
        Ok(u64::from_be_bytes(value))
    }

    fn byte(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn finished(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

fn open_plan_root(root: &Path) -> Result<File, String> {
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root)
        .map_err(|error| format!("unable to open MQTT plan directory: {error}"))?;
    let metadata = directory
        .metadata()
        .map_err(|error| format!("unable to inspect MQTT plan directory: {error}"))?;
    let current = fs::symlink_metadata(root)
        .map_err(|error| format!("unable to reinspect MQTT plan directory: {error}"))?;
    if current.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() < 2
        || metadata.mode() & 0o7777 != 0o700
        || current.dev() != metadata.dev()
        || current.ino() != metadata.ino()
    {
        return Err("MQTT plan directory identity is unsafe".to_string());
    }
    Ok(directory)
}

fn ensure_plan_root(root: &Path) -> Result<File, String> {
    match fs::create_dir(root) {
        Ok(()) => fs::set_permissions(root, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("unable to secure MQTT plan directory: {error}"))?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("unable to create MQTT plan directory: {error}")),
    }
    open_plan_root(root)
}

fn plan_path(root: &Path, instance: &str) -> Result<PathBuf, String> {
    if !safe_name(instance) || instance.len() > 64 {
        return Err("MQTT plan contains an unsafe instance".to_string());
    }
    Ok(root.join(format!("{instance}.plan")))
}

fn inspect_plan_file(path: &Path) -> Result<Option<fs::Metadata>, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.nlink() != 1
                || metadata.mode() & 0o7777 != 0o600
                || metadata.len() > MAX_PLAN_BYTES as u64
            {
                return Err(format!("MQTT plan file {} is unsafe", path.display()));
            }
            Ok(Some(metadata))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("unable to inspect MQTT plan file: {error}")),
    }
}

fn write_plan_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_PLAN_BYTES {
        return Err("MQTT plan exceeds its byte bound".to_string());
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| format!("unable to create MQTT plan file: {error}"))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("unable to persist MQTT plan file: {error}"))
}

fn publish_plan_files(root: &Path, plans: &[(String, Vec<u8>)]) -> Result<(), String> {
    let directory = ensure_plan_root(root)?;
    cleanup_abandoned_plan_files(root)?;
    validate_plan_root_entries(root)?;
    let directory_device = directory
        .metadata()
        .map_err(|error| format!("unable to inspect MQTT plan directory: {error}"))?
        .dev();
    let mut expected = BTreeSet::new();
    let mut targets = Vec::with_capacity(plans.len());
    for (instance, bytes) in plans {
        if !expected.insert(instance.clone()) {
            return Err("MQTT service plan contains a duplicate instance".to_string());
        }
        let final_path = plan_path(root, instance)?;
        if let Some(metadata) = inspect_plan_file(&final_path)? {
            if metadata.dev() != directory_device {
                return Err("MQTT plan file is on an unexpected device".to_string());
            }
        }
        targets.push((instance, bytes, final_path));
    }
    let mut staged = Vec::new();
    for (instance, bytes, final_path) in targets {
        let temporary = root.join(format!(
            ".staged_{instance}_{}_{}",
            std::process::id(),
            PLAN_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        if let Err(error) = write_plan_file(&temporary, bytes) {
            for (staged_path, _) in &staged {
                let _ = fs::remove_file(staged_path);
            }
            return Err(error);
        }
        staged.push((temporary, final_path));
    }
    let commit = (|| {
        for (temporary, final_path) in &staged {
            fs::rename(temporary, final_path)
                .map_err(|error| format!("unable to publish MQTT plan file: {error}"))?;
        }
        directory
            .sync_all()
            .map_err(|error| format!("unable to sync MQTT plan directory: {error}"))?;
        cleanup_stale_plan_files(root, &expected)
    })();
    if commit.is_err() {
        for (temporary, _) in &staged {
            let _ = fs::remove_file(temporary);
        }
    }
    commit
}

fn cleanup_abandoned_plan_files(root: &Path) -> Result<(), String> {
    for entry in fs::read_dir(root)
        .map_err(|error| format!("unable to enumerate MQTT plan directory: {error}"))?
    {
        let entry = entry.map_err(|error| format!("unable to inspect MQTT plan entry: {error}"))?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            return Err("MQTT plan directory contains a non-UTF-8 entry".to_string());
        };
        if !name.starts_with(".staged_") {
            continue;
        }
        if name.len() > 192
            || !name[8..]
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err("MQTT plan directory contains an unsafe staged entry".to_string());
        }
        inspect_plan_file(&entry.path())?
            .ok_or_else(|| "staged MQTT plan disappeared during cleanup".to_string())?;
        fs::remove_file(entry.path())
            .map_err(|error| format!("unable to remove abandoned MQTT plan: {error}"))?;
    }
    Ok(())
}

fn validate_plan_root_entries(root: &Path) -> Result<(), String> {
    for entry in fs::read_dir(root)
        .map_err(|error| format!("unable to enumerate MQTT plan directory: {error}"))?
    {
        let entry = entry.map_err(|error| format!("unable to inspect MQTT plan entry: {error}"))?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            return Err("MQTT plan directory contains a non-UTF-8 entry".to_string());
        };
        let Some(instance) = name.strip_suffix(".plan") else {
            return Err("MQTT plan directory contains an unknown entry".to_string());
        };
        if !safe_name(instance) || instance.len() > 64 {
            return Err("MQTT plan directory contains an unsafe entry".to_string());
        }
        inspect_plan_file(&entry.path())?
            .ok_or_else(|| "MQTT plan entry disappeared during validation".to_string())?;
    }
    Ok(())
}

fn cleanup_stale_plan_files(root: &Path, expected: &BTreeSet<String>) -> Result<(), String> {
    let entries = fs::read_dir(root)
        .map_err(|error| format!("unable to enumerate MQTT plan directory: {error}"))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("unable to inspect MQTT plan entry: {error}"))?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            return Err("MQTT plan directory contains a non-UTF-8 entry".to_string());
        };
        let Some(instance) = name.strip_suffix(".plan") else {
            return Err("MQTT plan directory contains an unknown entry".to_string());
        };
        if !safe_name(instance) || instance.len() > 64 {
            return Err("MQTT plan directory contains an unsafe entry".to_string());
        }
        inspect_plan_file(&entry.path())?
            .ok_or_else(|| "MQTT plan entry disappeared during cleanup".to_string())?;
        if !expected.contains(instance) {
            fs::remove_file(entry.path())
                .map_err(|error| format!("unable to remove stale MQTT plan: {error}"))?;
        }
    }
    Ok(())
}

fn cleanup_plan_root(root: &Path) -> Result<(), String> {
    match fs::symlink_metadata(root) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("unable to inspect MQTT plan directory: {error}")),
    }
    let directory = open_plan_root(root)?;
    cleanup_abandoned_plan_files(root)?;
    cleanup_stale_plan_files(root, &BTreeSet::new())?;
    directory
        .sync_all()
        .map_err(|error| format!("unable to sync MQTT plan cleanup: {error}"))?;
    fs::remove_dir(root).map_err(|error| format!("unable to remove MQTT plan directory: {error}"))
}

fn read_service_plan(root: &Path, instance: &str) -> Result<MqttPublisherConfig, String> {
    open_plan_root(root)?;
    let path = plan_path(root, instance)?;
    inspect_plan_file(&path)?.ok_or_else(|| format!("MQTT plan for {instance} is missing"))?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|error| format!("unable to open MQTT plan: {error}"))?;
    let before = file
        .metadata()
        .map_err(|error| format!("unable to inspect MQTT plan: {error}"))?;
    let mut bytes = Vec::with_capacity(before.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_PLAN_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("unable to read MQTT plan: {error}"))?;
    let after = file
        .metadata()
        .map_err(|error| format!("unable to reinspect MQTT plan: {error}"))?;
    let current = inspect_plan_file(&path)?
        .ok_or_else(|| "MQTT plan disappeared while it was read".to_string())?;
    if bytes.len() > MAX_PLAN_BYTES
        || before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || after.dev() != current.dev()
        || after.ino() != current.ino()
    {
        return Err("MQTT plan changed while it was read".to_string());
    }
    let config = MqttPublisherConfig::decode_plan(&bytes)?;
    if config.instance != instance {
        return Err("MQTT plan instance does not match its file name".to_string());
    }
    Ok(config)
}

fn option<'a>(section: &'a UciSection, name: &str) -> Option<&'a str> {
    section.options.get(name).map(String::as_str)
}

fn bool_option(section: &UciSection, name: &str, fallback: bool) -> Result<bool, String> {
    match option(section, name) {
        None => Ok(fallback),
        Some("0") => Ok(false),
        Some("1") => Ok(true),
        Some(_) => Err(format!("MQTT option {name} must be 0 or 1")),
    }
}

fn validate_text(value: &str, limit: usize) -> Result<(), String> {
    if value.len() > limit || value.contains('\0') || value.chars().any(char::is_control) {
        return Err("MQTT configuration contains unsafe or oversized text".to_string());
    }
    Ok(())
}

fn required_text(section: &UciSection, name: &str, limit: usize) -> Result<String, String> {
    let value = option(section, name).unwrap_or_default();
    validate_text(value, limit)?;
    if value.is_empty() {
        return Err(format!("MQTT option {name} is required"));
    }
    Ok(value.to_string())
}

fn optional_text(section: &UciSection, name: &str, limit: usize) -> Result<String, String> {
    let value = option(section, name).unwrap_or_default();
    validate_text(value, limit)?;
    Ok(value.to_string())
}

fn text_value(
    section: &UciSection,
    name: &str,
    fallback: &str,
    limit: usize,
) -> Result<String, String> {
    let value = option(section, name)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback);
    validate_text(value, limit)?;
    Ok(value.to_string())
}

fn topic_value(section: &UciSection, name: &str, fallback: &str) -> Result<String, String> {
    let value = text_value(section, name, fallback, MAX_TOPIC_BYTES)?;
    if validate_topic(&value).is_err() {
        return Err(format!("MQTT option {name} is not a safe topic prefix"));
    }
    Ok(value)
}

fn validate_topic(value: &str) -> Result<(), String> {
    validate_text(value, MAX_TOPIC_BYTES)?;
    if value.is_empty()
        || value.starts_with('/')
        || value.ends_with('/')
        || value.contains("//")
        || value.contains(['#', '+'])
    {
        return Err("MQTT plan contains an unsafe topic prefix".to_string());
    }
    Ok(())
}

fn safe_identifier(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

#[derive(Clone, Debug, Default, PartialEq)]
struct SummaryAccumulator {
    count: u64,
    dl_rate: f64,
    ul_rate: f64,
    dl_delays: f64,
    ul_delays: f64,
    dl_owd: f64,
    ul_owd: f64,
    dl_load: String,
    ul_load: String,
    cake_dl: f64,
    cake_ul: f64,
    event_epoch: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct CpuAccumulator {
    count: u64,
    total: f64,
    cores: Vec<f64>,
}

#[derive(Clone, Debug)]
pub(crate) struct MqttAggregator {
    minimum_interval: Duration,
    publish_cpu: bool,
    last_emit: Option<Duration>,
    summary: SummaryAccumulator,
    cpu: CpuAccumulator,
}

impl MqttAggregator {
    pub(crate) fn new(minimum_interval: Duration, publish_cpu: bool) -> Self {
        Self {
            minimum_interval,
            publish_cpu,
            last_emit: None,
            summary: SummaryAccumulator::default(),
            cpu: CpuAccumulator::default(),
        }
    }

    pub(crate) fn observe_line(
        &mut self,
        line: &str,
        monotonic_now: Duration,
    ) -> Result<Option<String>, String> {
        if line.len() > 64 * 1024 || line.chars().any(|value| value == '\0') {
            return Err("MQTT log record is unsafe or oversized".to_string());
        }
        let fields = line.split("; ").collect::<Vec<_>>();
        match fields.first().copied() {
            Some("SUMMARY") if fields.len() >= 12 => self.observe_summary(&fields)?,
            Some("CPU") if self.publish_cpu && fields.len() >= 3 => self.observe_cpu(&fields)?,
            _ => return Ok(None),
        }
        if self.summary.count == 0 {
            return Ok(None);
        }
        let due = self.last_emit.is_none_or(|last| {
            monotonic_now
                .checked_sub(last)
                .is_none_or(|elapsed| elapsed >= self.minimum_interval)
        });
        if !due {
            return Ok(None);
        }
        self.last_emit = Some(monotonic_now);
        Ok(Some(self.take_state_json()))
    }

    fn observe_summary(&mut self, fields: &[&str]) -> Result<(), String> {
        self.summary.event_epoch = finite(fields[1], "event epoch")?;
        self.summary.dl_rate += finite(fields[2], "download rate")?;
        self.summary.ul_rate += finite(fields[3], "upload rate")?;
        self.summary.dl_delays += finite(fields[4], "download delay sum")?;
        self.summary.ul_delays += finite(fields[5], "upload delay sum")?;
        self.summary.dl_owd += finite(fields[6], "download OWD")?;
        self.summary.ul_owd += finite(fields[7], "upload OWD")?;
        validate_text(fields[8], 64)?;
        validate_text(fields[9], 64)?;
        self.summary.dl_load = fields[8].to_string();
        self.summary.ul_load = fields[9].to_string();
        self.summary.cake_dl = finite(fields[10], "CAKE download rate")?;
        self.summary.cake_ul = finite(fields[11], "CAKE upload rate")?;
        self.summary.count = self.summary.count.saturating_add(1);
        Ok(())
    }

    fn observe_cpu(&mut self, fields: &[&str]) -> Result<(), String> {
        let core_count = fields.len().saturating_sub(3);
        if core_count > MAX_CPU_CORES {
            return Err("MQTT CPU record has too many cores".to_string());
        }
        if self.cpu.count > 0 && self.cpu.cores.len() != core_count {
            return Err("MQTT CPU core count changed within one window".to_string());
        }
        if self.cpu.cores.is_empty() {
            self.cpu.cores.resize(core_count, 0.0);
        }
        self.cpu.total += finite(fields[2], "CPU total")?;
        for (index, value) in fields.iter().skip(3).enumerate() {
            self.cpu.cores[index] += finite(value, "CPU core")?;
        }
        self.cpu.count = self.cpu.count.saturating_add(1);
        Ok(())
    }

    fn take_state_json(&mut self) -> String {
        let count = self.summary.count.max(1);
        let divisor = count as f64;
        let mut cpu = String::new();
        if self.publish_cpu && self.cpu.count > 0 {
            let cpu_divisor = self.cpu.count as f64;
            cpu.push_str(&format!(
                ",\"cpu_total\":{:.1}",
                self.cpu.total / cpu_divisor
            ));
            for (index, value) in self.cpu.cores.iter().enumerate() {
                cpu.push_str(&format!(",\"cpu_core{index}\":{:.1}", value / cpu_divisor));
            }
        }
        let output = format!(
            concat!(
                "{{\"event_epoch\":{:.6},\"dl_achieved_rate_kbps\":{:.1},",
                "\"ul_achieved_rate_kbps\":{:.1},\"dl_sum_delays\":{:.1},",
                "\"ul_sum_delays\":{:.1},\"dl_avg_owd_delta_us\":{:.1},",
                "\"ul_avg_owd_delta_us\":{:.1},\"dl_load_condition\":\"{}\",",
                "\"ul_load_condition\":\"{}\",\"cake_dl_rate_kbps\":{:.0},",
                "\"cake_ul_rate_kbps\":{:.0}{},\"samples\":{}}}"
            ),
            self.summary.event_epoch,
            self.summary.dl_rate / divisor,
            self.summary.ul_rate / divisor,
            self.summary.dl_delays / divisor,
            self.summary.ul_delays / divisor,
            self.summary.dl_owd / divisor,
            self.summary.ul_owd / divisor,
            json_escape(&self.summary.dl_load),
            json_escape(&self.summary.ul_load),
            self.summary.cake_dl,
            self.summary.cake_ul,
            cpu,
            count,
        );
        self.summary = SummaryAccumulator::default();
        self.cpu = CpuAccumulator::default();
        output
    }
}

fn finite(value: &str, name: &str) -> Result<f64, String> {
    let parsed = value
        .parse::<f64>()
        .map_err(|_| format!("MQTT {name} is invalid"))?;
    if !parsed.is_finite() {
        return Err(format!("MQTT {name} is not finite"));
    }
    Ok(parsed)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MqttPublish {
    pub(crate) topic: String,
    pub(crate) payload: String,
    pub(crate) retain: bool,
}

pub(crate) fn discovery_messages(
    config: &MqttPublisherConfig,
    cpu_cores: usize,
) -> Result<Vec<MqttPublish>, String> {
    if cpu_cores > MAX_CPU_CORES {
        return Err("MQTT CPU discovery exceeds its core limit".to_string());
    }
    let mut sensors = vec![
        (
            "dl_achieved_rate_kbps",
            "DL Achieved Rate",
            "{{ value_json.dl_achieved_rate_kbps }}",
            "kbps",
            "dl_achieved_rate",
        ),
        (
            "ul_achieved_rate_kbps",
            "UL Achieved Rate",
            "{{ value_json.ul_achieved_rate_kbps }}",
            "kbps",
            "ul_achieved_rate",
        ),
        (
            "cake_dl_rate_kbps",
            "CAKE DL Rate",
            "{{ value_json.cake_dl_rate_kbps }}",
            "kbps",
            "cake_dl_rate",
        ),
        (
            "cake_ul_rate_kbps",
            "CAKE UL Rate",
            "{{ value_json.cake_ul_rate_kbps }}",
            "kbps",
            "cake_ul_rate",
        ),
        (
            "dl_sum_delays",
            "DL Delay Sum",
            "{{ value_json.dl_sum_delays }}",
            "us",
            "dl_delay",
        ),
        (
            "ul_sum_delays",
            "UL Delay Sum",
            "{{ value_json.ul_sum_delays }}",
            "us",
            "ul_delay",
        ),
        (
            "dl_avg_owd_delta_us",
            "DL OWD Delta",
            "{{ value_json.dl_avg_owd_delta_us }}",
            "us",
            "dl_owd",
        ),
        (
            "ul_avg_owd_delta_us",
            "UL OWD Delta",
            "{{ value_json.ul_avg_owd_delta_us }}",
            "us",
            "ul_owd",
        ),
        (
            "dl_load_condition",
            "DL Load Condition",
            "{{ value_json.dl_load_condition }}",
            "",
            "dl_condition",
        ),
        (
            "ul_load_condition",
            "UL Load Condition",
            "{{ value_json.ul_load_condition }}",
            "",
            "ul_condition",
        ),
    ];
    if config.publish_cpu {
        sensors.push((
            "cpu_total",
            "CPU Total",
            "{{ value_json.cpu_total }}",
            "%",
            "cpu_total",
        ));
    }
    let mut messages = Vec::new();
    for (object_id, name, template, unit, unique) in sensors {
        messages.push(discovery_message(
            config, object_id, name, template, unit, unique,
        ));
    }
    if config.publish_cpu {
        for core in 0..cpu_cores {
            let object = format!("cpu_core{core}");
            let name = format!("CPU Core {core}");
            let template = format!("{{{{ value_json.cpu_core{core} }}}}");
            let unique = format!("cpu_core{core}");
            messages.push(discovery_message(
                config, &object, &name, &template, "%", &unique,
            ));
        }
    }
    Ok(messages)
}

fn discovery_message(
    config: &MqttPublisherConfig,
    object_id: &str,
    name: &str,
    template: &str,
    unit: &str,
    unique: &str,
) -> MqttPublish {
    let unit = if unit.is_empty() {
        String::new()
    } else {
        format!(",\"unit_of_measurement\":\"{}\"", json_escape(unit))
    };
    MqttPublish {
        topic: format!(
            "{}/sensor/{}/{object_id}/config",
            config.discovery_prefix, config.device_id
        ),
        payload: format!(
            concat!(
                "{{\"name\":\"{}\",\"state_topic\":\"{}\",",
                "\"value_template\":\"{}\"{},\"unique_id\":\"{}_{}\",",
                "\"availability_topic\":\"{}\",\"device\":{{\"identifiers\":[\"{}\"],",
                "\"name\":\"{}\"}}}}"
            ),
            json_escape(name),
            json_escape(&config.state_topic()),
            json_escape(template),
            unit,
            json_escape(&config.device_id),
            json_escape(unique),
            json_escape(&config.availability_topic()),
            json_escape(&config.device_id),
            json_escape(&config.device_name),
        ),
        retain: true,
    }
}

fn push_utf8(buffer: &mut Vec<u8>, value: &str) -> Result<(), String> {
    let length = u16::try_from(value.len()).map_err(|_| "MQTT string is too long".to_string())?;
    buffer.extend_from_slice(&length.to_be_bytes());
    buffer.extend_from_slice(value.as_bytes());
    Ok(())
}

fn push_remaining_length(buffer: &mut Vec<u8>, mut value: usize) -> Result<(), String> {
    if value > 268_435_455 {
        return Err("MQTT remaining length exceeds the protocol maximum".to_string());
    }
    loop {
        let mut encoded = (value % 128) as u8;
        value /= 128;
        if value > 0 {
            encoded |= 0x80;
        }
        buffer.push(encoded);
        if value == 0 {
            return Ok(());
        }
    }
}

pub(crate) fn connect_packet(
    config: &MqttPublisherConfig,
    client_id: &str,
) -> Result<Vec<u8>, String> {
    validate_text(client_id, 64)?;
    let mut body = Vec::new();
    push_utf8(&mut body, "MQTT")?;
    body.push(4);
    let mut flags = 0x02 | 0x04 | 0x08 | 0x20;
    if !config.username.is_empty() {
        flags |= 0x80;
    }
    if !config.password.is_empty() {
        flags |= 0x40;
    }
    body.push(flags);
    body.extend_from_slice(&MQTT_KEEPALIVE_SECONDS.to_be_bytes());
    push_utf8(&mut body, client_id)?;
    push_utf8(&mut body, &config.availability_topic())?;
    push_utf8(&mut body, "offline")?;
    if !config.username.is_empty() {
        push_utf8(&mut body, &config.username)?;
    }
    if !config.password.is_empty() {
        push_utf8(&mut body, &config.password)?;
    }
    if body.len() > MAX_MQTT_PACKET_BYTES {
        return Err("MQTT CONNECT packet is oversized".to_string());
    }
    let mut packet = vec![0x10];
    push_remaining_length(&mut packet, body.len())?;
    packet.extend_from_slice(&body);
    Ok(packet)
}

pub(crate) fn publish_packet(
    topic: &str,
    payload: &[u8],
    retain: bool,
    packet_id: u16,
) -> Result<Vec<u8>, String> {
    if packet_id == 0 {
        return Err("MQTT packet identifier must be non-zero".to_string());
    }
    validate_text(topic, MAX_TOPIC_BYTES)?;
    if topic.is_empty() || topic.contains(['#', '+']) {
        return Err("MQTT publish topic is invalid".to_string());
    }
    let mut body = Vec::new();
    push_utf8(&mut body, topic)?;
    body.extend_from_slice(&packet_id.to_be_bytes());
    body.extend_from_slice(payload);
    if body.len() > MAX_MQTT_PACKET_BYTES {
        return Err("MQTT PUBLISH packet is oversized".to_string());
    }
    let mut packet = vec![if retain { 0x33 } else { 0x32 }];
    push_remaining_length(&mut packet, body.len())?;
    packet.extend_from_slice(&body);
    Ok(packet)
}

struct MqttClient {
    stream: TcpStream,
    next_packet_id: u16,
}

fn mqtt_client_id(instance: &str, pid: u32) -> String {
    let identity = digest(&SHA256, instance.as_bytes());
    let short = identity.as_ref()[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("cake_{short}_{pid}")
}

impl MqttClient {
    fn connect(config: &MqttPublisherConfig) -> Result<Self, String> {
        let addresses = (config.host.as_str(), config.port)
            .to_socket_addrs()
            .map_err(|error| format!("unable to resolve MQTT broker: {error}"))?
            .take(MAX_RESOLVED_ADDRESSES)
            .collect::<Vec<_>>();
        if addresses.is_empty() {
            return Err("MQTT broker resolved to no addresses".to_string());
        }
        let mut last_error = None;
        let connect_started = Instant::now();
        for address in addresses {
            let remaining = MQTT_CONNECT_TIMEOUT.saturating_sub(connect_started.elapsed());
            if remaining.is_zero() {
                break;
            }
            match TcpStream::connect_timeout(&address, remaining) {
                Ok(mut stream) => {
                    stream
                        .set_read_timeout(Some(MQTT_IO_TIMEOUT))
                        .map_err(|error| format!("unable to bound MQTT reads: {error}"))?;
                    stream
                        .set_write_timeout(Some(MQTT_IO_TIMEOUT))
                        .map_err(|error| format!("unable to bound MQTT writes: {error}"))?;
                    let _ = stream.set_nodelay(true);
                    let client_id = mqtt_client_id(&config.instance, std::process::id());
                    stream
                        .write_all(&connect_packet(config, &client_id)?)
                        .map_err(|error| format!("unable to send MQTT CONNECT: {error}"))?;
                    let (header, body) = read_packet(&mut stream)?;
                    if header != 0x20 || body.len() != 2 || body[0] != 0 || body[1] != 0 {
                        return Err("MQTT broker rejected the CONNECT request".to_string());
                    }
                    return Ok(Self {
                        stream,
                        next_packet_id: 1,
                    });
                }
                Err(error) => last_error = Some(error),
            }
        }
        Err(format!(
            "unable to connect to MQTT broker: {}",
            last_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "no address was attempted".to_string())
        ))
    }

    fn publish(&mut self, topic: &str, payload: &[u8], retain: bool) -> Result<(), String> {
        let packet_id = self.next_packet_id.max(1);
        self.next_packet_id = self.next_packet_id.wrapping_add(1).max(1);
        self.stream
            .write_all(&publish_packet(topic, payload, retain, packet_id)?)
            .map_err(|error| format!("unable to send MQTT PUBLISH: {error}"))?;
        for _ in 0..8 {
            let (header, body) = read_packet(&mut self.stream)?;
            match header & 0xf0 {
                0x40 if header == 0x40 && body == packet_id.to_be_bytes() => return Ok(()),
                0xd0 if header == 0xd0 && body.is_empty() => continue,
                0xe0 if header == 0xe0 => return Err("MQTT broker disconnected".to_string()),
                _ => return Err("MQTT broker returned an unexpected packet".to_string()),
            }
        }
        Err("MQTT broker did not acknowledge a publication".to_string())
    }

    fn ping(&mut self) -> Result<(), String> {
        self.stream
            .write_all(&[0xc0, 0x00])
            .map_err(|error| format!("unable to send MQTT PINGREQ: {error}"))?;
        let (header, body) = read_packet(&mut self.stream)?;
        if header != 0xd0 || !body.is_empty() {
            return Err("MQTT broker returned an invalid PINGRESP".to_string());
        }
        Ok(())
    }

    fn read_unsolicited(&mut self) -> Result<(), String> {
        let (header, body) = read_packet(&mut self.stream)?;
        match header & 0xf0 {
            0xd0 if header == 0xd0 && body.is_empty() => Ok(()),
            0xe0 if header == 0xe0 => Err("MQTT broker disconnected".to_string()),
            _ => Err("MQTT broker sent an unexpected unsolicited packet".to_string()),
        }
    }

    fn disconnect(&mut self) -> Result<(), String> {
        self.stream
            .write_all(&[0xe0, 0x00])
            .map_err(|error| format!("unable to send MQTT DISCONNECT: {error}"))
    }
}

impl AsRawFd for MqttClient {
    fn as_raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }
}

fn read_packet(stream: &mut TcpStream) -> Result<(u8, Vec<u8>), String> {
    let mut header = [0u8; 1];
    stream
        .read_exact(&mut header)
        .map_err(|error| format!("unable to read MQTT packet header: {error}"))?;
    let mut multiplier = 1usize;
    let mut remaining = 0usize;
    for index in 0..4 {
        let mut byte = [0u8; 1];
        stream
            .read_exact(&mut byte)
            .map_err(|error| format!("unable to read MQTT remaining length: {error}"))?;
        remaining = remaining
            .checked_add(usize::from(byte[0] & 0x7f).saturating_mul(multiplier))
            .ok_or_else(|| "MQTT remaining length overflowed".to_string())?;
        if byte[0] & 0x80 == 0 {
            if remaining > MAX_MQTT_PACKET_BYTES {
                return Err("MQTT packet exceeds the bounded receive limit".to_string());
            }
            let mut body = vec![0u8; remaining];
            stream
                .read_exact(&mut body)
                .map_err(|error| format!("unable to read MQTT packet body: {error}"))?;
            return Ok((header[0], body));
        }
        if index == 3 {
            return Err("MQTT remaining length is malformed".to_string());
        }
        multiplier = multiplier.saturating_mul(128);
    }
    Err("MQTT remaining length is malformed".to_string())
}

struct SignalEvents(OwnedFd);

impl SignalEvents {
    fn new() -> Result<Self, String> {
        let mut mask = unsafe { std::mem::zeroed::<libc::sigset_t>() };
        if unsafe { libc::sigemptyset(&mut mask) } != 0
            || unsafe { libc::sigaddset(&mut mask, libc::SIGINT) } != 0
            || unsafe { libc::sigaddset(&mut mask, libc::SIGTERM) } != 0
        {
            return Err(format!(
                "unable to construct MQTT signal mask: {}",
                io::Error::last_os_error()
            ));
        }
        let block = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut()) };
        if block != 0 {
            return Err(format!(
                "unable to block MQTT process signals: {}",
                io::Error::from_raw_os_error(block)
            ));
        }
        let raw = unsafe { libc::signalfd(-1, &mask, libc::SFD_NONBLOCK | libc::SFD_CLOEXEC) };
        if raw < 0 {
            return Err(format!(
                "unable to create MQTT signal descriptor: {}",
                io::Error::last_os_error()
            ));
        }
        Ok(Self(unsafe { OwnedFd::from_raw_fd(raw) }))
    }

    fn drain(&self) -> Result<(), String> {
        let mut info = unsafe { std::mem::zeroed::<libc::signalfd_siginfo>() };
        let read = unsafe {
            libc::read(
                self.0.as_raw_fd(),
                (&mut info as *mut libc::signalfd_siginfo).cast(),
                std::mem::size_of::<libc::signalfd_siginfo>(),
            )
        };
        if read as usize != std::mem::size_of::<libc::signalfd_siginfo>() {
            return Err(format!(
                "unable to read MQTT signal descriptor: {}",
                io::Error::last_os_error()
            ));
        }
        Ok(())
    }
}

impl AsRawFd for SignalEvents {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

struct MonotonicTimer(OwnedFd);

impl MonotonicTimer {
    fn periodic(interval: Duration) -> Result<Self, String> {
        if interval.is_zero() {
            return Err("MQTT keepalive interval must be non-zero".to_string());
        }
        let raw = unsafe {
            libc::timerfd_create(
                libc::CLOCK_MONOTONIC,
                libc::TFD_NONBLOCK | libc::TFD_CLOEXEC,
            )
        };
        if raw < 0 {
            return Err(format!(
                "unable to create MQTT keepalive timer: {}",
                io::Error::last_os_error()
            ));
        }
        let descriptor = unsafe { OwnedFd::from_raw_fd(raw) };
        let specification = libc::itimerspec {
            it_interval: duration_timespec(interval),
            it_value: duration_timespec(interval),
        };
        if unsafe {
            libc::timerfd_settime(
                descriptor.as_raw_fd(),
                0,
                &specification,
                std::ptr::null_mut(),
            )
        } != 0
        {
            return Err(format!(
                "unable to arm MQTT keepalive timer: {}",
                io::Error::last_os_error()
            ));
        }
        Ok(Self(descriptor))
    }

    fn drain(&self) -> Result<(), String> {
        let mut expirations = 0u64;
        let read = unsafe {
            libc::read(
                self.0.as_raw_fd(),
                (&mut expirations as *mut u64).cast(),
                std::mem::size_of::<u64>(),
            )
        };
        if read as usize != std::mem::size_of::<u64>() {
            return Err(format!(
                "unable to read MQTT keepalive timer: {}",
                io::Error::last_os_error()
            ));
        }
        Ok(())
    }
}

impl AsRawFd for MonotonicTimer {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

fn duration_timespec(value: Duration) -> libc::timespec {
    libc::timespec {
        tv_sec: value.as_secs().min(libc::time_t::MAX as u64) as libc::time_t,
        tv_nsec: libc::c_long::try_from(value.subsec_nanos())
            .expect("a normalized Duration nanosecond field fits in c_long"),
    }
}

struct LogEvents {
    descriptor: OwnedFd,
    file_watch: Option<i32>,
}

impl LogEvents {
    fn new(path: &Path) -> Result<Self, String> {
        let raw = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if raw < 0 {
            return Err(format!(
                "unable to create MQTT log watcher: {}",
                io::Error::last_os_error()
            ));
        }
        let mut watcher = Self {
            descriptor: unsafe { OwnedFd::from_raw_fd(raw) },
            file_watch: None,
        };
        let parent = path
            .parent()
            .ok_or_else(|| "MQTT log path has no parent".to_string())?;
        watcher.add_watch(
            parent,
            libc::IN_CREATE | libc::IN_MOVED_TO | libc::IN_ATTRIB,
        )?;
        watcher.watch_file(path)?;
        Ok(watcher)
    }

    fn add_watch(&self, path: &Path, mask: u32) -> Result<i32, String> {
        let encoded = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| "MQTT log watch path contains NUL".to_string())?;
        let watch =
            unsafe { libc::inotify_add_watch(self.descriptor.as_raw_fd(), encoded.as_ptr(), mask) };
        if watch < 0 {
            return Err(format!(
                "unable to watch MQTT log {}: {}",
                path.display(),
                io::Error::last_os_error()
            ));
        }
        Ok(watch)
    }

    fn watch_file(&mut self, path: &Path) -> Result<(), String> {
        if let Some(watch) = self.file_watch.take() {
            let _ = unsafe { libc::inotify_rm_watch(self.descriptor.as_raw_fd(), watch) };
        }
        self.file_watch = Some(self.add_watch(
            path,
            libc::IN_MODIFY | libc::IN_MOVE_SELF | libc::IN_DELETE_SELF | libc::IN_ATTRIB,
        )?);
        Ok(())
    }

    fn drain(&self) -> Result<(), String> {
        let mut buffer = [0u8; 16 * 1024];
        loop {
            let read = unsafe {
                libc::read(
                    self.descriptor.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            };
            if read > 0 {
                continue;
            }
            if read == 0 {
                return Err("MQTT inotify descriptor reached EOF".to_string());
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                return Ok(());
            }
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("unable to read MQTT log events: {error}"));
        }
    }
}

impl AsRawFd for LogEvents {
    fn as_raw_fd(&self) -> RawFd {
        self.descriptor.as_raw_fd()
    }
}

struct LogFollower {
    path: PathBuf,
    file: File,
    device: u64,
    inode: u64,
    offset: u64,
    partial: Vec<u8>,
}

impl LogFollower {
    fn open(path: PathBuf) -> Result<Self, String> {
        let parent = path
            .parent()
            .ok_or_else(|| "MQTT log path has no parent".to_string())?;
        let parent_metadata = fs::symlink_metadata(parent)
            .map_err(|error| format!("unable to inspect MQTT log directory: {error}"))?;
        if parent_metadata.file_type().is_symlink()
            || !parent_metadata.is_dir()
            || parent_metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err("MQTT log directory identity is unsafe".to_string());
        }
        match fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(&path)
                    .map_err(|error| format!("unable to create MQTT log file: {error}"))?;
            }
            Err(error) => return Err(format!("unable to inspect MQTT log file: {error}")),
        }
        let (mut file, metadata) = open_log_file(&path)?;
        let offset = file
            .seek(SeekFrom::End(0))
            .map_err(|error| format!("unable to seek MQTT log: {error}"))?;
        Ok(Self {
            path,
            file,
            device: metadata.dev(),
            inode: metadata.ino(),
            offset,
            partial: Vec::new(),
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn read_available(&mut self) -> Result<(Vec<String>, bool), String> {
        let reopened = self.refresh_identity()?;
        let mut chunk = Vec::new();
        Read::by_ref(&mut self.file)
            .take((MAX_LOG_READ_BYTES + 1) as u64)
            .read_to_end(&mut chunk)
            .map_err(|error| format!("unable to read MQTT log: {error}"))?;
        if chunk.len() > MAX_LOG_READ_BYTES {
            return Err("MQTT log event exceeds its bounded read limit".to_string());
        }
        self.offset = self.offset.saturating_add(chunk.len() as u64);
        if self.partial.len().saturating_add(chunk.len()) > MAX_PARTIAL_LINE_BYTES {
            return Err("MQTT log contains an oversized unterminated record".to_string());
        }
        self.partial.extend_from_slice(&chunk);
        let mut lines = Vec::new();
        let mut consumed = 0usize;
        while let Some(relative) = self.partial[consumed..]
            .iter()
            .position(|byte| *byte == b'\n')
        {
            let end = consumed + relative;
            let raw = self.partial[consumed..end]
                .strip_suffix(b"\r")
                .unwrap_or(&self.partial[consumed..end]);
            let line =
                std::str::from_utf8(raw).map_err(|_| "MQTT log record is not UTF-8".to_string())?;
            lines.push(line.to_string());
            consumed = end + 1;
        }
        if consumed > 0 {
            self.partial.drain(..consumed);
        }
        Ok((lines, reopened))
    }

    fn refresh_identity(&mut self) -> Result<bool, String> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(format!("unable to inspect MQTT log: {error}")),
        };
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Err("MQTT log path is not a regular file".to_string());
        }
        if metadata.dev() != self.device || metadata.ino() != self.inode {
            let (file, metadata) = open_log_file(&self.path)?;
            self.file = file;
            self.device = metadata.dev();
            self.inode = metadata.ino();
            self.offset = 0;
            self.partial.clear();
            return Ok(true);
        }
        if metadata.len() < self.offset {
            self.file
                .seek(SeekFrom::Start(0))
                .map_err(|error| format!("unable to follow truncated MQTT log: {error}"))?;
            self.offset = 0;
            self.partial.clear();
        }
        Ok(false)
    }
}

fn open_log_file(path: &Path) -> Result<(File, fs::Metadata), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("unable to inspect MQTT log: {error}"))?;
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
    {
        return Err("MQTT log path is not a regular file".to_string());
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| format!("unable to open MQTT log: {error}"))?;
    let opened = file
        .metadata()
        .map_err(|error| format!("unable to attest opened MQTT log: {error}"))?;
    if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
        return Err("MQTT log identity changed while opening".to_string());
    }
    Ok((file, opened))
}

fn cpu_core_count() -> Result<usize, String> {
    let path = std::env::var_os("CAKE_AUTORATE_PROC_STAT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/proc/stat"));
    let data = fs::read(&path).map_err(|error| format!("unable to read CPU topology: {error}"))?;
    if data.len() > 1024 * 1024 {
        return Err("CPU topology file exceeds its bounded limit".to_string());
    }
    let text = std::str::from_utf8(&data).map_err(|_| "CPU topology is not UTF-8".to_string())?;
    let cores = text
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| {
            name.strip_prefix("cpu").is_some_and(|suffix| {
                !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
            })
        })
        .count();
    if cores == 0 || cores > MAX_CPU_CORES {
        return Err("CPU topology has an invalid core count".to_string());
    }
    Ok(cores)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::thread;

    static NEXT_FIXTURE: AtomicU32 = AtomicU32::new(1);

    fn temp_root(label: &str) -> PathBuf {
        let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("cake-mqtt-{label}-{}-{id}", std::process::id()));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    fn section(extra: &[(&str, &str)]) -> UciSection {
        let mut options = BTreeMap::from([
            ("enabled".to_string(), "1".to_string()),
            ("mqtt_enabled".to_string(), "1".to_string()),
            ("mqtt_host".to_string(), "broker.invalid".to_string()),
            ("log_to_file".to_string(), "1".to_string()),
            ("output_summary_stats".to_string(), "1".to_string()),
        ]);
        for (name, value) in extra {
            options.insert((*name).to_string(), (*value).to_string());
        }
        UciSection {
            section_type: "cake_autorate".to_string(),
            options,
        }
    }

    fn config() -> MqttPublisherConfig {
        MqttPublisherConfig::from_section("wan", &section(&[]))
            .unwrap()
            .unwrap()
    }

    #[test]
    fn private_service_plans_are_sorted_canonical_and_require_a_live_controller() {
        let root = temp_root("plans");
        let mut package = UciPackage::default();
        package.sections.insert(
            "wan_b".to_string(),
            section(&[
                ("mqtt_username", "publisher"),
                ("mqtt_password", "private passphrase"),
            ]),
        );
        package.sections.insert("wan_a".to_string(), section(&[]));
        assert_eq!(
            publish_service_plans(&package, &root).unwrap(),
            ["wan_a", "wan_b"]
        );
        assert_eq!(
            read_service_plan(&root, "wan_a").unwrap(),
            MqttPublisherConfig::from_section("wan_a", &section(&[]))
                .unwrap()
                .unwrap()
        );
        assert_eq!(
            read_service_plan(&root, "wan_b").unwrap().password,
            "private passphrase"
        );
        let mode = fs::metadata(root.join("wan_b.plan")).unwrap().mode() & 0o7777;
        assert_eq!(mode, 0o600);
        package
            .sections
            .get_mut("wan_a")
            .unwrap()
            .options
            .insert("enabled".to_string(), "0".to_string());
        assert!(publish_service_plans(&package, &root).is_err());
        assert!(read_service_plan(&root, "wan_b").is_ok());
        cleanup_plan_root(&root).unwrap();
        assert!(!root.exists());
    }

    #[test]
    fn private_service_plan_rejects_tamper_and_instance_mismatch() {
        let root = temp_root("plan-tamper");
        let mut package = UciPackage::default();
        package.sections.insert("wan".to_string(), section(&[]));
        publish_service_plans(&package, &root).unwrap();
        let path = root.join("wan.plan");
        let mut bytes = fs::read(&path).unwrap();
        bytes.push(0);
        fs::write(&path, &bytes).unwrap();
        assert!(read_service_plan(&root, "wan").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn service_plan_recovers_owned_staging_but_rejects_unknown_entries() {
        let root = temp_root("plan-recovery");
        let staged = root.join(".staged_wan_42_7");
        fs::write(&staged, b"incomplete").unwrap();
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o600)).unwrap();
        let mut package = UciPackage::default();
        package.sections.insert("wan".to_string(), section(&[]));
        assert_eq!(publish_service_plans(&package, &root).unwrap(), ["wan"]);
        assert!(!staged.exists());

        let foreign = root.join("foreign");
        fs::write(&foreign, b"preserve").unwrap();
        fs::set_permissions(&foreign, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(publish_service_plans(&package, &root).is_err());
        assert_eq!(fs::read(&foreign).unwrap(), b"preserve");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn config_rejects_the_shells_silent_zero_interval_and_missing_sources() {
        assert!(MqttPublisherConfig::from_section(
            "wan",
            &section(&[("mqtt_min_interval_s", "nonnumeric")])
        )
        .is_err());
        assert!(MqttPublisherConfig::from_section(
            "wan",
            &section(&[("mqtt_min_interval_s", "0")])
        )
        .is_err());
        assert!(MqttPublisherConfig::from_section(
            "wan",
            &section(&[("mqtt_publish_cpu_stats", "1")])
        )
        .is_err());
        assert!(MqttPublisherConfig::from_section(
            "wan",
            &section(&[("mqtt_base_topic", "unsafe/#")])
        )
        .is_err());
        assert!(
            MqttPublisherConfig::from_section("wan", &section(&[("mqtt_password", "secret")]))
                .is_err()
        );
        let longest = "a".repeat(64);
        let client_id = mqtt_client_id(&longest, u32::MAX);
        assert!(client_id.len() <= 64);
        assert_ne!(client_id, mqtt_client_id(&"b".repeat(64), u32::MAX));
    }

    #[test]
    fn aggregation_preserves_summary_means_and_does_not_reuse_stale_cpu() {
        let mut aggregate = MqttAggregator::new(Duration::from_secs(2), true);
        assert!(aggregate
            .observe_line("CPU; 100.0; 25.0; 10.0; 40.0", Duration::ZERO)
            .unwrap()
            .is_none());
        let first = aggregate
            .observe_line(
                "SUMMARY; 100.0; 1000; 200; 1; 2; 3; 4; high_dl; low_ul; 900; 180",
                Duration::ZERO,
            )
            .unwrap()
            .unwrap();
        assert!(first.contains("\"cpu_total\":25.0"));
        assert!(first.contains("\"cpu_core0\":10.0"));
        assert!(first.contains("\"samples\":1"));

        assert!(aggregate
            .observe_line(
                "SUMMARY; 101.0; 2000; 400; 3; 4; 5; 6; high_dl; low_ul; 1800; 360",
                Duration::from_secs(1),
            )
            .unwrap()
            .is_none());
        let second = aggregate
            .observe_line(
                "SUMMARY; 102.0; 4000; 800; 5; 6; 7; 8; high_dl; low_ul; 3600; 720",
                Duration::from_secs(2),
            )
            .unwrap()
            .unwrap();
        assert!(second.contains("\"dl_achieved_rate_kbps\":3000.0"));
        assert!(second.contains("\"samples\":2"));
        assert!(!second.contains("cpu_total"));
    }

    #[test]
    fn discovery_and_packets_preserve_retained_qos_one_contract() {
        let cfg = config();
        let discovery = discovery_messages(&cfg, 0).unwrap();
        assert_eq!(discovery.len(), 10);
        assert!(discovery.iter().all(|message| message.retain));
        assert!(discovery[0].topic.starts_with("homeassistant/sensor/"));
        assert!(discovery[0].payload.contains("\"availability_topic\""));

        let connect = connect_packet(&cfg, "cake_wan_1").unwrap();
        assert_eq!(connect[0], 0x10);
        assert!(connect.windows(7).any(|window| window == b"offline"));
        let retained =
            publish_packet("cake-autorate/wan/availability", b"online", true, 1).unwrap();
        let state = publish_packet("cake-autorate/wan", b"{}", false, 2).unwrap();
        assert_eq!(retained[0], 0x33);
        assert_eq!(state[0], 0x32);
        assert!(publish_packet("unsafe/+", b"x", false, 3).is_err());
    }

    #[test]
    fn native_client_completes_connect_publish_ping_and_disconnect() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let (header, connect) = read_packet(&mut stream).unwrap();
            assert_eq!(header, 0x10);
            assert!(connect.windows(7).any(|window| window == b"offline"));
            stream.write_all(&[0x20, 0x02, 0x00, 0x00]).unwrap();

            let (header, publish) = read_packet(&mut stream).unwrap();
            assert_eq!(header, 0x33);
            let topic_len = usize::from(u16::from_be_bytes([publish[0], publish[1]]));
            let packet_at = 2 + topic_len;
            let packet_id = [publish[packet_at], publish[packet_at + 1]];
            stream
                .write_all(&[0x40, 0x02, packet_id[0], packet_id[1]])
                .unwrap();

            let (header, body) = read_packet(&mut stream).unwrap();
            assert_eq!((header, body), (0xc0, Vec::new()));
            stream.write_all(&[0xd0, 0x00]).unwrap();
            let (header, body) = read_packet(&mut stream).unwrap();
            assert_eq!((header, body), (0xe0, Vec::new()));
        });

        let mut cfg = config();
        cfg.host = "127.0.0.1".to_string();
        cfg.port = port;
        let mut client = MqttClient::connect(&cfg).unwrap();
        client
            .publish(&cfg.availability_topic(), b"online", true)
            .unwrap();
        client.ping().unwrap();
        client.disconnect().unwrap();
        server.join().unwrap();
    }

    #[test]
    fn log_follower_starts_at_eof_and_follows_inode_replacement() {
        let root = temp_root("follower");
        let path = root.join("cake-autorate.wan.log");
        fs::write(&path, b"SUMMARY; old\n").unwrap();
        let mut follower = LogFollower::open(path.clone()).unwrap();

        let mut writer = OpenOptions::new().append(true).open(&path).unwrap();
        writer.write_all(b"SUMMARY; new\n").unwrap();
        writer.flush().unwrap();
        let (lines, reopened) = follower.read_available().unwrap();
        assert!(!reopened);
        assert_eq!(lines, ["SUMMARY; new"]);

        fs::rename(&path, root.join("rotated.log")).unwrap();
        fs::write(&path, b"CPU; replacement\n").unwrap();
        let (lines, reopened) = follower.read_available().unwrap();
        assert!(reopened);
        assert_eq!(lines, ["CPU; replacement"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn duration_timespec_preserves_a_normalized_nanosecond_field() {
        let value = Duration::new(7, 999_999_999);
        let timespec = duration_timespec(value);
        assert_eq!(timespec.tv_sec, 7);
        assert_eq!(timespec.tv_nsec, 999_999_999);
    }
}
