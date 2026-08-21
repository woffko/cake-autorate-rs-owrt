//! Full-package pinger discovery and reflector planning for the LuCI wizard.
//!
//! The controller remains the only owner of live latency probes.  This command
//! performs bounded, one-shot discovery and returns a recommendation; it never
//! edits UCI or starts a persistent pinger.

use super::json_wire::{bool_json, json_escape};
use super::process::{run_bounded_command_output, BoundedCommandOutput, SpawnSpec};
use crate::reflector_defaults::standard_reflectors;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsString;
use std::net::Ipv6Addr;
use std::path::{Path, PathBuf};
use std::time::Duration;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const FPING_SCAN_TIMEOUT: Duration = Duration::from_secs(45);
const TSPING_SCAN_SECONDS: &str = "4";
const APK_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_OUTPUT_BYTES: usize = 256 * 1024;
// Keep the longest routed tsping argv below process::MAX_ARGUMENTS (64).
const MAX_CANDIDATES: usize = 48;
const MAX_WARNINGS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Status,
    Scan,
    Install,
}

impl Mode {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "status" | "" => Ok(Self::Status),
            "scan" => Ok(Self::Scan),
            "install" => Ok(Self::Install),
            _ => Err("unsupported mode".to_string()),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Scan => "scan",
            Self::Install => "install",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PingerMethod {
    Fping,
    FpingTs,
    Tsping,
    Irtt,
    Ping,
}

impl PingerMethod {
    const ALL: [Self; 5] = [
        Self::Fping,
        Self::FpingTs,
        Self::Tsping,
        Self::Irtt,
        Self::Ping,
    ];

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "fping" | "" => Ok(Self::Fping),
            "fping-ts" => Ok(Self::FpingTs),
            "tsping" => Ok(Self::Tsping),
            "irtt" => Ok(Self::Irtt),
            "ping" => Ok(Self::Ping),
            _ => Err(format!("Unknown pinger backend: {value}")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Fping => "fping",
            Self::FpingTs => "fping-ts",
            Self::Tsping => "tsping",
            Self::Irtt => "irtt",
            Self::Ping => "ping",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Ping => "ping fallback",
            _ => self.as_str(),
        }
    }

    fn delay_type(self) -> &'static str {
        match self {
            Self::Fping | Self::Ping => "RTT",
            Self::FpingTs | Self::Tsping | Self::Irtt => "OWD",
        }
    }

    fn target_mode(self) -> &'static str {
        match self {
            Self::Fping | Self::FpingTs | Self::Tsping => "round-robin",
            Self::Ping | Self::Irtt => "individual",
        }
    }

    fn package(self) -> &'static str {
        match self {
            Self::Fping | Self::FpingTs => "fping",
            Self::Irtt => "irtt",
            Self::Tsping | Self::Ping => "",
        }
    }

    fn installable(self) -> bool {
        matches!(self, Self::Fping | Self::FpingTs | Self::Irtt)
    }

    fn install_hint(self) -> &'static str {
        match self {
            Self::Fping | Self::FpingTs => "apk add fping",
            Self::Tsping => "install a compatible tsping binary manually",
            Self::Irtt => {
                "apk add irtt, then configure at least one irtt_server and keep router/server clocks synchronized"
            }
            Self::Ping => "provided by the base system",
        }
    }

    fn quality(self) -> u8 {
        match self {
            Self::Fping => 70,
            Self::FpingTs | Self::Tsping => 90,
            Self::Irtt => 100,
            Self::Ping => 30,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RouteMode {
    Main,
    Mwan3,
}

impl RouteMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Mwan3 => "mwan3",
        }
    }
}

#[derive(Clone, Debug)]
struct Settings {
    section: String,
    mode: Mode,
    route_mode: RouteMode,
    mwan3_member: String,
    configured_method: String,
    configured_no_pingers: usize,
    reflector_ping_interval_s: String,
    configured_reflectors: Vec<String>,
    irtt_servers: Vec<String>,
    invalid_irtt_servers: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Availability {
    fping: bool,
    fping_ts: bool,
    tsping: bool,
    irtt: bool,
    ping: bool,
}

impl Availability {
    fn binary_available(&self, method: PingerMethod) -> bool {
        match method {
            PingerMethod::Fping => self.fping,
            PingerMethod::FpingTs => self.fping_ts,
            PingerMethod::Tsping => self.tsping,
            PingerMethod::Irtt => self.irtt,
            PingerMethod::Ping => self.ping,
        }
    }

    fn available(&self, method: PingerMethod, irtt_servers: usize) -> bool {
        self.binary_available(method) && (method != PingerMethod::Irtt || irtt_servers > 0)
    }

    fn reason(&self, method: PingerMethod, irtt_servers: usize) -> String {
        match method {
            PingerMethod::Fping if self.fping => "available".to_string(),
            PingerMethod::Fping => "not installed".to_string(),
            PingerMethod::FpingTs if self.fping_ts => "available".to_string(),
            PingerMethod::FpingTs => "requires fping with --icmp-timestamp".to_string(),
            PingerMethod::Tsping if self.tsping => "available".to_string(),
            PingerMethod::Tsping => "not installed or bounded timeout is unavailable".to_string(),
            PingerMethod::Irtt if !self.irtt => "not installed".to_string(),
            PingerMethod::Irtt if irtt_servers == 0 => {
                "installed; configure at least one irtt_server and keep clocks synchronized"
                    .to_string()
            }
            PingerMethod::Irtt => format!(
                "available with {irtt_servers} configured server(s); requires synchronized clocks"
            ),
            PingerMethod::Ping if self.ping => "available".to_string(),
            PingerMethod::Ping => "not installed".to_string(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct ProbeObservation {
    rtt_ms: Option<f64>,
    timestamp_rtt_ms: Option<f64>,
}

#[derive(Clone, Debug)]
struct PingerPlan {
    settings: Settings,
    availability: Availability,
    candidate_source: String,
    candidates: Vec<String>,
    valid_candidates: Vec<String>,
    observations: BTreeMap<String, ProbeObservation>,
    timestamp_probe_backend: String,
    recommended: PingerMethod,
    recommended_no_pingers: usize,
    recommended_reason: String,
    active: Vec<String>,
    spare: Vec<String>,
    rtt_reflectors: Vec<String>,
    bad: Vec<String>,
    warnings: Vec<String>,
}

impl PingerPlan {
    fn encode_json(&self) -> String {
        let rtt_ok_count = self
            .observations
            .values()
            .filter(|item| item.rtt_ms.is_some())
            .count();
        let timestamp_ok_count = self
            .observations
            .values()
            .filter(|item| item.timestamp_rtt_ms.is_some())
            .count();
        let recommended_reflectors = self
            .active
            .iter()
            .chain(self.spare.iter())
            .cloned()
            .collect::<Vec<_>>();
        let mut backends = String::new();
        for method in PingerMethod::ALL {
            if !backends.is_empty() {
                backends.push(',');
            }
            let available = self
                .availability
                .available(method, self.settings.irtt_servers.len());
            backends.push_str(&format!(
                concat!(
                    "{{\"name\":\"{}\",\"title\":\"{}\",\"delay_type\":\"{}\",",
                    "\"target_mode\":\"{}\",\"available\":{},\"supported\":true,",
                    "\"installable\":{},\"package\":\"{}\",\"quality\":{},",
                    "\"recommended\":{},\"configured\":{},\"reason\":\"{}\",",
                    "\"install_hint\":\"{}\"}}"
                ),
                method.as_str(),
                method.title(),
                method.delay_type(),
                method.target_mode(),
                bool_json(available),
                bool_json(method.installable()),
                method.package(),
                method.quality(),
                bool_json(method == self.recommended),
                bool_json(method.as_str() == self.settings.configured_method),
                json_escape(
                    &self
                        .availability
                        .reason(method, self.settings.irtt_servers.len())
                ),
                json_escape(method.install_hint())
            ));
        }
        let mut output = format!(
            concat!(
                "{{\"section\":\"{}\",\"mode\":\"{}\",\"route_mode\":\"{}\",",
                "\"mwan3_member\":\"{}\",\"configured_method\":\"{}\",",
                "\"configured_no_pingers\":{},\"configured_irtt_server_count\":{},",
                "\"reflector_ping_interval_s\":\"{}\",\"candidate_source\":\"{}\",",
                "\"default_pool_count\":{},\"candidate_count\":{},\"valid_count\":{},",
                "\"rtt_ok_count\":{},\"timestamp_ok_count\":{},",
                "\"timestamp_probe_backend\":\"{}\",\"recommended_method\":\"{}\",",
                "\"recommended_no_pingers\":{},\"recommended_reason\":\"{}\",",
                "\"backends\":[{}],\"active\":{},\"spare\":{},",
                "\"recommended_reflectors\":{},\"rtt_reflectors\":{},",
                "\"bad\":{},\"warnings\":{}"
            ),
            json_escape(&self.settings.section),
            self.settings.mode.as_str(),
            self.settings.route_mode.as_str(),
            json_escape(&self.settings.mwan3_member),
            json_escape(&self.settings.configured_method),
            self.settings.configured_no_pingers,
            self.settings.irtt_servers.len(),
            json_escape(&self.settings.reflector_ping_interval_s),
            self.candidate_source,
            standard_reflectors().len(),
            self.candidates.len(),
            self.valid_candidates.len(),
            rtt_ok_count,
            timestamp_ok_count,
            self.timestamp_probe_backend,
            self.recommended.as_str(),
            self.recommended_no_pingers,
            json_escape(&self.recommended_reason),
            backends,
            string_array(&self.active),
            string_array(&self.spare),
            string_array(&recommended_reflectors),
            string_array(&self.rtt_reflectors),
            string_array(&self.bad),
            string_array(&self.warnings)
        );
        if self.settings.mode == Mode::Scan {
            output.push_str(",\"reflectors\":[");
            let mut first = true;
            for host in &self.valid_candidates {
                if !first {
                    output.push(',');
                }
                first = false;
                let observation = self.observations.get(host).cloned().unwrap_or_default();
                output.push_str(&format!(
                    "{{\"host\":\"{}\",\"rtt_ok\":{},\"timestamp_ok\":{},\"rtt_ms\":{},\"timestamp_rtt_ms\":{}}}",
                    json_escape(host),
                    bool_json(observation.rtt_ms.is_some()),
                    bool_json(observation.timestamp_rtt_ms.is_some()),
                    optional_number(observation.rtt_ms),
                    optional_number(observation.timestamp_rtt_ms)
                ));
            }
            output.push(']');
        }
        output.push_str("}\n");
        output
    }
}

#[derive(Clone, Debug)]
struct Environment {
    uci: PathBuf,
    fping: Option<PathBuf>,
    ping: Option<PathBuf>,
    tsping: Option<PathBuf>,
    irtt: Option<PathBuf>,
    timeout: Option<PathBuf>,
    mwan3: Option<PathBuf>,
    nft: Option<PathBuf>,
    ubus: Option<PathBuf>,
    apk: Option<PathBuf>,
}

impl Environment {
    fn live() -> Result<Self, String> {
        Ok(Self {
            uci: required_program("CAKE_AUTORATE_UCI_BIN", "uci")?,
            fping: optional_program("CAKE_AUTORATE_FPING_BIN", "fping"),
            ping: optional_program("CAKE_AUTORATE_PING_BIN", "ping"),
            tsping: optional_program("CAKE_AUTORATE_TSPING_BIN", "tsping"),
            irtt: optional_program("CAKE_AUTORATE_IRTT_BIN", "irtt"),
            timeout: optional_program("CAKE_AUTORATE_TIMEOUT_BIN", "timeout"),
            mwan3: optional_program("CAKE_AUTORATE_MWAN3_BIN", "mwan3"),
            nft: optional_program("CAKE_AUTORATE_NFT_BIN", "nft"),
            ubus: optional_program("CAKE_AUTORATE_UBUS_BIN", "ubus"),
            apk: optional_program("CAKE_AUTORATE_APK_BIN", "apk"),
        })
    }
}

fn fixed_search_paths(name: &str) -> impl Iterator<Item = PathBuf> + '_ {
    ["/usr/sbin", "/usr/bin", "/sbin", "/bin"]
        .into_iter()
        .map(move |root| Path::new(root).join(name))
}

fn optional_program(variable: &str, name: &str) -> Option<PathBuf> {
    if let Some(value) = env::var_os(variable) {
        let path = PathBuf::from(value);
        return path.is_file().then_some(path);
    }
    fixed_search_paths(name).find(|path| path.is_file())
}

fn required_program(variable: &str, name: &str) -> Result<PathBuf, String> {
    optional_program(variable, name).ok_or_else(|| format!("{name} is unavailable"))
}

fn string_array(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| format!("\"{}\"", json_escape(value)))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn optional_number(value: Option<f64>) -> String {
    value
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string())
}

fn command(
    program: &Path,
    arguments: impl IntoIterator<Item = impl Into<OsString>>,
    timeout: Duration,
) -> Result<BoundedCommandOutput, String> {
    run_bounded_command_output(
        &SpawnSpec {
            program: program.to_path_buf(),
            arguments: arguments.into_iter().map(Into::into).collect(),
            environment: Vec::new(),
        },
        timeout,
        MAX_OUTPUT_BYTES,
        || false,
    )
}

fn output_text(output: &[u8], label: &str) -> Result<String, String> {
    String::from_utf8(output.to_vec()).map_err(|_| format!("{label} returned non-UTF-8 output"))
}

fn uci_get(environment: &Environment, section: &str, option: &str) -> Result<String, String> {
    let output = command(
        &environment.uci,
        [
            "-q".to_string(),
            "get".to_string(),
            format!("cake-autorate.{section}.{option}"),
        ],
        COMMAND_TIMEOUT,
    )?;
    if !output.status.success() {
        return Ok(String::new());
    }
    Ok(output_text(&output.stdout, "uci")?.trim().to_string())
}

fn safe_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn safe_route_member(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'@' | b'-')
        })
}

fn valid_hostname(value: &str) -> bool {
    value.len() <= 253
        && !value.contains("..")
        && value.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn valid_reflector(value: &str) -> bool {
    if value.is_empty()
        || value.starts_with('-')
        || value.len() > 253
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'.' | b'-'))
    {
        return false;
    }
    if value.contains(':') {
        return value.parse::<Ipv6Addr>().is_ok();
    }
    if value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        return value.parse::<std::net::Ipv4Addr>().is_ok();
    }
    valid_hostname(value)
}

fn valid_irtt_server(value: &str) -> bool {
    if valid_reflector(value) {
        return true;
    }
    if !value.starts_with('[') || value.contains("://") {
        return false;
    }
    let Some(close) = value.find(']') else {
        return false;
    };
    if value[1..close].parse::<Ipv6Addr>().is_err() {
        return false;
    }
    let suffix = &value[close + 1..];
    suffix.is_empty()
        || suffix
            .strip_prefix(':')
            .and_then(|port| port.parse::<u16>().ok())
            .is_some_and(|port| port > 0)
}

fn unique_validated(
    values: impl IntoIterator<Item = String>,
    irtt: bool,
) -> (Vec<String>, Vec<String>) {
    let mut accepted = Vec::new();
    let mut rejected = Vec::new();
    let mut seen = BTreeSet::new();
    for value in values.into_iter().take(MAX_CANDIDATES) {
        let valid = if irtt {
            valid_irtt_server(&value)
        } else {
            valid_reflector(&value)
        };
        if !valid {
            rejected.push(value);
        } else if seen.insert(value.clone()) {
            accepted.push(value);
        }
    }
    (accepted, rejected)
}

fn push_warning(warnings: &mut Vec<String>, value: impl Into<String>) {
    if warnings.len() < MAX_WARNINGS {
        warnings.push(value.into());
    }
}

fn load_settings(
    environment: &Environment,
    section: &str,
    mode: Mode,
    route_override: &str,
    member_override: &str,
) -> Result<Settings, String> {
    if !safe_name(section) {
        return Err("invalid pinger-plan section".to_string());
    }
    let configured_method = {
        let value = uci_get(environment, section, "pinger_method")?;
        if value.is_empty() {
            "fping".to_string()
        } else {
            value
        }
    };
    let configured_no_pingers = uci_get(environment, section, "no_pingers")?
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
        .unwrap_or(6);
    let reflector_ping_interval_s = {
        let value = uci_get(environment, section, "reflector_ping_interval_s")?;
        if value.is_empty() {
            "0.3".to_string()
        } else {
            value
        }
    };
    let configured_reflectors = uci_get(environment, section, "reflector")?
        .split_ascii_whitespace()
        .map(str::to_string)
        .collect();
    let raw_irtt = uci_get(environment, section, "irtt_server")?
        .split_ascii_whitespace()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let (irtt_servers, invalid_irtt_servers) = unique_validated(raw_irtt, true);
    let configured_route = if route_override.is_empty() {
        let value = uci_get(environment, section, "route_mode")?;
        if value.is_empty() {
            "auto".to_string()
        } else {
            value
        }
    } else {
        route_override.to_string()
    };
    let mwan3_member = if member_override.is_empty() {
        uci_get(environment, section, "mwan3_member")?
    } else {
        member_override.to_string()
    };
    let route_mode = match configured_route.as_str() {
        "main" => RouteMode::Main,
        "mwan3" => RouteMode::Mwan3,
        "auto" if mwan3_member.is_empty() => RouteMode::Main,
        "auto" => RouteMode::Mwan3,
        _ => return Err(format!("Invalid route_mode: {configured_route}")),
    };
    if route_mode == RouteMode::Mwan3 && !safe_route_member(&mwan3_member) {
        return Err("Invalid mwan3_member".to_string());
    }
    Ok(Settings {
        section: section.to_string(),
        mode,
        route_mode,
        mwan3_member,
        configured_method,
        configured_no_pingers,
        reflector_ping_interval_s,
        configured_reflectors,
        irtt_servers,
        invalid_irtt_servers,
    })
}

fn check_route(environment: &Environment, settings: &Settings) -> Result<(), String> {
    if settings.route_mode != RouteMode::Mwan3 {
        return Ok(());
    }
    let mwan3 = environment
        .mwan3
        .as_deref()
        .ok_or_else(|| "mwan3 routing is selected but mwan3 is unavailable".to_string())?;
    let nft = environment
        .nft
        .as_deref()
        .ok_or_else(|| "nftables table inet mwan3 is unavailable".to_string())?;
    let table = command(nft, ["list", "table", "inet", "mwan3"], COMMAND_TIMEOUT)?;
    if !table.status.success() {
        return Err("nftables table inet mwan3 is unavailable".to_string());
    }
    if settings.mode == Mode::Scan {
        let ubus = environment
            .ubus
            .as_deref()
            .ok_or_else(|| "ubus is unavailable for mwan3 status".to_string())?;
        let request = format!("{{\"interface\":\"{}\"}}", settings.mwan3_member);
        let status = command(
            ubus,
            ["call", "mwan3", "status", request.as_str()],
            COMMAND_TIMEOUT,
        )?;
        let stdout = output_text(&status.stdout, "mwan3 status")?;
        if !status.status.success()
            || super::runtime_health::json_string_value(&stdout, "status").as_deref()
                != Some("online")
        {
            return Err(format!(
                "mwan3 member {} is offline or unavailable",
                settings.mwan3_member
            ));
        }
    }
    // The existence check above is intentionally kept even though the path is
    // used only by routed probes below.
    let _ = mwan3;
    Ok(())
}

fn detect_availability(environment: &Environment) -> Result<Availability, String> {
    let fping = environment.fping.is_some();
    let fping_ts = if let Some(path) = environment.fping.as_deref() {
        let help = command(path, ["--help"], COMMAND_TIMEOUT)?;
        let mut text = output_text(&help.stdout, "fping --help")?;
        text.push_str(&output_text(&help.stderr, "fping --help")?);
        text.contains("--icmp-timestamp")
    } else {
        false
    };
    Ok(Availability {
        fping,
        fping_ts,
        // tsping is intentionally considered usable only when the bounded
        // base-system timeout command is also present.
        tsping: environment.tsping.is_some() && environment.timeout.is_some(),
        irtt: environment.irtt.is_some(),
        ping: environment.ping.is_some(),
    })
}

fn routed_command(
    environment: &Environment,
    settings: &Settings,
    program: &Path,
    arguments: Vec<OsString>,
    timeout: Duration,
) -> Result<BoundedCommandOutput, String> {
    if settings.route_mode == RouteMode::Main {
        return command(program, arguments, timeout);
    }
    let mwan3 = environment
        .mwan3
        .as_deref()
        .ok_or_else(|| "mwan3 is unavailable".to_string())?;
    let mut routed = vec![
        OsString::from("use"),
        OsString::from(&settings.mwan3_member),
        OsString::from("exec"),
        program.as_os_str().to_os_string(),
    ];
    routed.extend(arguments);
    command(mwan3, routed, timeout)
}

fn parse_positive_float(value: &str) -> Option<f64> {
    value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0)
}

fn parse_fping(output: &str) -> BTreeMap<String, f64> {
    let mut results = BTreeMap::new();
    for line in output.lines() {
        if !line.contains(": xmt/rcv/%loss = ") || !line.contains("min/avg/max =") {
            continue;
        }
        let Some((host, _)) = line.split_once(": xmt/rcv/%loss = ") else {
            continue;
        };
        let Some((_, values)) = line.split_once("min/avg/max =") else {
            continue;
        };
        let Some(average) = values
            .trim()
            .split('/')
            .nth(1)
            .and_then(parse_positive_float)
        else {
            continue;
        };
        let host = host.trim();
        if valid_reflector(host) {
            results.entry(host.to_string()).or_insert(average);
        }
    }
    results
}

fn parse_tsping(output: &str) -> BTreeMap<String, f64> {
    let mut results = BTreeMap::new();
    for line in output.lines() {
        let fields = line.split(',').map(str::trim).collect::<Vec<_>>();
        if fields.len() != 10 || !valid_reflector(fields[1]) {
            continue;
        }
        let Some(download) = parse_positive_float(fields[8]) else {
            continue;
        };
        let Some(upload) = parse_positive_float(fields[9]) else {
            continue;
        };
        results
            .entry(fields[1].to_string())
            .or_insert(download + upload);
    }
    results
}

fn probe_fping(
    environment: &Environment,
    settings: &Settings,
    targets: &[String],
    timestamp: bool,
) -> Result<BTreeMap<String, f64>, String> {
    let Some(program) = environment.fping.as_deref() else {
        return Ok(BTreeMap::new());
    };
    if targets.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut arguments = Vec::<OsString>::new();
    if timestamp {
        arguments.push("--icmp-timestamp".into());
    }
    arguments.extend(["-i", "100", "-c", "1", "-t", "1000", "--"].map(Into::into));
    arguments.extend(targets.iter().map(OsString::from));
    let output = routed_command(
        environment,
        settings,
        program,
        arguments,
        FPING_SCAN_TIMEOUT,
    )?;
    let mut text = output_text(&output.stdout, "fping scan")?;
    text.push_str(&output_text(&output.stderr, "fping scan")?);
    Ok(parse_fping(&text))
}

fn probe_tsping(
    environment: &Environment,
    settings: &Settings,
    targets: &[String],
) -> Result<BTreeMap<String, f64>, String> {
    let (Some(timeout), Some(tsping)) = (
        environment.timeout.as_deref(),
        environment.tsping.as_deref(),
    ) else {
        return Ok(BTreeMap::new());
    };
    if targets.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut arguments = vec![OsString::from(TSPING_SCAN_SECONDS)];
    if settings.route_mode == RouteMode::Mwan3 {
        let mwan3 = environment
            .mwan3
            .as_deref()
            .ok_or_else(|| "mwan3 is unavailable".to_string())?;
        arguments.extend([
            mwan3.as_os_str().to_os_string(),
            OsString::from("use"),
            OsString::from(&settings.mwan3_member),
            OsString::from("exec"),
        ]);
    }
    arguments.push(tsping.as_os_str().to_os_string());
    arguments.extend(
        [
            "--print-timestamps",
            "--machine-readable=,",
            "--sleep-time",
            "0",
            "--target-spacing",
            "50",
        ]
        .map(Into::into),
    );
    arguments.extend(targets.iter().map(OsString::from));
    let output = command(timeout, arguments, Duration::from_secs(10))?;
    Ok(parse_tsping(&output_text(&output.stdout, "tsping scan")?))
}

fn build_plan(
    settings: Settings,
    availability: Availability,
    rtt: BTreeMap<String, f64>,
    timestamp: BTreeMap<String, f64>,
    timestamp_probe_backend: String,
) -> PingerPlan {
    let defaults = standard_reflectors();
    let candidate_source = if settings.configured_reflectors.is_empty() {
        "upstream-default"
    } else if settings.mode == Mode::Scan {
        "configured-plus-upstream-default"
    } else {
        "configured"
    }
    .to_string();
    let mut candidate_values = settings.configured_reflectors.clone();
    if candidate_values.is_empty() || settings.mode == Mode::Scan {
        candidate_values.extend(defaults);
    }
    let candidate_limit_exceeded = candidate_values.len() > MAX_CANDIDATES;
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();
    for candidate in candidate_values.into_iter().take(MAX_CANDIDATES) {
        if seen.insert(candidate.clone()) {
            candidates.push(candidate);
        }
    }
    let (valid_candidates, mut bad) = unique_validated(candidates.clone(), false);
    let mut warnings = Vec::new();
    if candidate_limit_exceeded {
        push_warning(
            &mut warnings,
            format!("Reflector candidates exceed the {MAX_CANDIDATES}-entry limit."),
        );
    }
    for invalid in &bad {
        push_warning(
            &mut warnings,
            format!("Ignoring invalid reflector candidate: {invalid}"),
        );
    }
    for invalid in &settings.invalid_irtt_servers {
        push_warning(
            &mut warnings,
            format!("Ignoring invalid irtt_server candidate: {invalid}"),
        );
    }
    if !availability.fping {
        push_warning(
            &mut warnings,
            "fping is not installed; concurrent RTT probing is unavailable.",
        );
    }
    if !availability.fping_ts {
        push_warning(
            &mut warnings,
            "Installed fping does not expose --icmp-timestamp; fping-ts is unavailable.",
        );
    }
    if !availability.tsping {
        push_warning(
            &mut warnings,
            "tsping binary or bounded timeout is unavailable; tsping remains a manual optional backend.",
        );
    }
    if !availability.irtt {
        push_warning(
            &mut warnings,
            "irtt package is not installed; pinger_method=irtt is unavailable until irtt is installed and irtt_server is configured.",
        );
    } else if settings.irtt_servers.is_empty() {
        push_warning(
            &mut warnings,
            "irtt binary is installed, but no irtt_server is configured; pinger_method=irtt is unavailable until at least one explicit IRTT server is set and clocks are synchronized.",
        );
    } else {
        push_warning(
            &mut warnings,
            "irtt OWD probing requires router and IRTT server clocks to be synchronized; unsynchronized clocks can produce negative one-way delays that are ignored.",
        );
    }

    let mut observations = BTreeMap::new();
    let mut rtt_reflectors = Vec::new();
    for host in &valid_candidates {
        let observation = ProbeObservation {
            rtt_ms: rtt.get(host).copied(),
            timestamp_rtt_ms: timestamp.get(host).copied(),
        };
        if observation.rtt_ms.is_some() {
            rtt_reflectors.push(host.clone());
        }
        observations.insert(host.clone(), observation);
    }
    let rtt_ok_count = observations
        .values()
        .filter(|value| value.rtt_ms.is_some())
        .count();
    let timestamp_ok_count = observations
        .values()
        .filter(|value| value.timestamp_rtt_ms.is_some())
        .count();
    let mut target = settings
        .configured_no_pingers
        .min(6)
        .min(valid_candidates.len())
        .max(1);
    let mut recommended = PingerMethod::Fping;
    let mut reason =
        "fping is the upstream default and the most reliable concurrent RTT backend.".to_string();
    if settings.mode == Mode::Scan {
        if availability.fping_ts && timestamp_ok_count >= target {
            recommended = PingerMethod::FpingTs;
            reason = "Enough reflectors answered ICMP timestamp probes; fping-ts can provide directional OWD."
                .to_string();
        } else if availability.tsping && timestamp_ok_count >= target {
            recommended = PingerMethod::Tsping;
            reason = "Enough reflectors answered ICMP timestamp probes; tsping can provide directional OWD."
                .to_string();
        } else if availability.fping && rtt_ok_count >= 1 {
            reason = "Timestamp-capable reflectors were insufficient; use concurrent RTT probing."
                .to_string();
        } else if availability.ping {
            recommended = PingerMethod::Ping;
            reason = "fping probing did not find usable reflectors; ping is an emergency per-reflector fallback."
                .to_string();
        } else {
            reason = "No usable pinger backend is available.".to_string();
        }
    } else {
        match PingerMethod::parse(&settings.configured_method) {
            Ok(PingerMethod::Irtt) => {
                recommended = PingerMethod::Irtt;
                reason = "Current configuration requests explicit IRTT servers.".to_string();
            }
            Ok(PingerMethod::Ping) => {
                recommended = PingerMethod::Ping;
                reason = "Current configuration uses the per-reflector ping fallback; run a scan before changing it."
                    .to_string();
            }
            Ok(method @ (PingerMethod::FpingTs | PingerMethod::Tsping)) => {
                recommended = method;
                reason = "Current configuration requests a timestamp-aware backend; run a scan to verify reflector support."
                    .to_string();
            }
            _ => {}
        }
    }
    if settings.configured_method == "irtt" {
        recommended = PingerMethod::Irtt;
        reason = "Current configuration requests explicit IRTT servers.".to_string();
        target = settings.configured_no_pingers.max(1);
        if !availability.irtt {
            reason = "Current configuration requests irtt, but the irtt package is not installed."
                .to_string();
        } else if settings.irtt_servers.is_empty() {
            reason = "Current configuration requests irtt, but no irtt_server is configured."
                .to_string();
        } else {
            target = target.min(settings.irtt_servers.len());
        }
    }

    let mut active = Vec::new();
    let mut spare = Vec::new();
    if recommended == PingerMethod::Irtt {
        for server in &settings.irtt_servers {
            if active.len() < target {
                active.push(server.clone());
            } else {
                spare.push(server.clone());
            }
        }
    } else {
        for host in &valid_candidates {
            let observation = observations.get(host).cloned().unwrap_or_default();
            let usable = if settings.mode == Mode::Scan {
                match recommended {
                    PingerMethod::FpingTs | PingerMethod::Tsping => {
                        observation.timestamp_rtt_ms.is_some()
                    }
                    PingerMethod::Fping | PingerMethod::Ping => observation.rtt_ms.is_some(),
                    PingerMethod::Irtt => false,
                }
            } else {
                true
            };
            if usable {
                if active.len() < target {
                    active.push(host.clone());
                } else {
                    spare.push(host.clone());
                }
            } else if settings.mode == Mode::Scan && !bad.contains(host) {
                bad.push(host.clone());
            }
        }
    }
    if settings.mode == Mode::Scan && active.len() < target {
        push_warning(
            &mut warnings,
            "Fewer usable reflectors than desired active pingers; reduce Pingers or add better reflectors.",
        );
    }
    PingerPlan {
        settings,
        availability,
        candidate_source,
        candidates,
        valid_candidates,
        observations,
        timestamp_probe_backend,
        recommended,
        recommended_no_pingers: target,
        recommended_reason: reason,
        active,
        spare,
        rtt_reflectors,
        bad,
        warnings,
    }
}

fn install(
    environment: &mut Environment,
    settings: &Settings,
    selected: &str,
) -> Result<String, String> {
    let selected = if selected.is_empty() {
        PingerMethod::parse(&settings.configured_method)?
    } else {
        PingerMethod::parse(selected)?
    };
    let mut availability = detect_availability(environment)?;
    let ready = availability.available(selected, settings.irtt_servers.len());
    if !selected.installable() {
        if ready {
            return Ok(install_json(
                selected,
                true,
                true,
                &availability.reason(selected, settings.irtt_servers.len()),
            ));
        }
        return Err(format!(
            "{} cannot be installed automatically: {}.",
            selected.title(),
            selected.install_hint()
        ));
    }
    if ready {
        return Ok(install_json(
            selected,
            true,
            true,
            &availability.reason(selected, settings.irtt_servers.len()),
        ));
    }
    let apk = environment
        .apk
        .as_deref()
        .ok_or_else(|| "apk was not found on this router.".to_string())?;
    let output = command(apk, ["add", selected.package()], APK_TIMEOUT)?;
    if !output.status.success() {
        let stderr = output_text(&output.stderr, "apk")?;
        let stdout = output_text(&output.stdout, "apk")?;
        return Err(format!(
            "Failed to install {}: {}",
            selected.package(),
            if stderr.trim().is_empty() {
                stdout.trim()
            } else {
                stderr.trim()
            }
        ));
    }
    environment.fping = optional_program("CAKE_AUTORATE_FPING_BIN", "fping");
    environment.irtt = optional_program("CAKE_AUTORATE_IRTT_BIN", "irtt");
    availability = detect_availability(environment)?;
    Ok(install_json(
        selected,
        true,
        availability.available(selected, settings.irtt_servers.len()),
        &availability.reason(selected, settings.irtt_servers.len()),
    ))
}

fn install_json(backend: PingerMethod, installed: bool, available: bool, reason: &str) -> String {
    format!(
        "{{\"backend\":\"{}\",\"backend_title\":\"{}\",\"package\":\"{}\",\"installed\":{},\"available\":{},\"supported\":true,\"reason\":\"{}\"}}\n",
        backend.as_str(),
        backend.title(),
        backend.package(),
        bool_json(installed),
        bool_json(available),
        json_escape(reason)
    )
}

fn run_with_environment(
    environment: &mut Environment,
    mut args: impl Iterator<Item = String>,
) -> Result<String, String> {
    let mut section = args.next().unwrap_or_else(|| "primary".to_string());
    let mut mode = args.next().unwrap_or_else(|| "status".to_string());
    if matches!(section.as_str(), "--status" | "status") {
        section = "primary".to_string();
        mode = "status".to_string();
    }
    let selected = args.next().unwrap_or_default();
    let route_override = args.next().unwrap_or_default();
    let member_override = args.next().unwrap_or_default();
    if args.next().is_some() {
        return Err("too many pinger-plan arguments".to_string());
    }
    let mode = Mode::parse(&mode)?;
    let settings = load_settings(
        environment,
        &section,
        mode,
        &route_override,
        &member_override,
    )?;
    check_route(environment, &settings)?;
    if mode == Mode::Install {
        return install(environment, &settings, &selected);
    }
    let availability = detect_availability(environment)?;
    let mut candidates = settings.configured_reflectors.clone();
    if candidates.is_empty() || mode == Mode::Scan {
        candidates.extend(standard_reflectors());
    }
    let (targets, _) = unique_validated(candidates, false);
    let (rtt, timestamp, timestamp_backend) = if mode == Mode::Scan {
        let rtt = probe_fping(environment, &settings, &targets, false).unwrap_or_default();
        let mut timestamp = BTreeMap::new();
        let mut backend = String::new();
        if availability.fping_ts {
            timestamp = probe_fping(environment, &settings, &targets, true).unwrap_or_default();
            backend = "fping-ts".to_string();
        } else if availability.tsping {
            timestamp = probe_tsping(environment, &settings, &targets).unwrap_or_default();
            backend = "tsping".to_string();
        }
        if timestamp.is_empty() && availability.tsping && backend != "tsping" {
            let fallback = probe_tsping(environment, &settings, &targets).unwrap_or_default();
            if !fallback.is_empty() {
                timestamp = fallback;
                backend = "tsping".to_string();
            }
        }
        (rtt, timestamp, backend)
    } else {
        (BTreeMap::new(), BTreeMap::new(), String::new())
    };
    Ok(build_plan(settings, availability, rtt, timestamp, timestamp_backend).encode_json())
}

pub(crate) fn run_pinger_plan(args: impl Iterator<Item = String>) -> Result<String, String> {
    run_with_environment(&mut Environment::live()?, args)
}

pub(crate) fn error_json(error: &str) -> String {
    format!("{{\"error\":\"{}\"}}\n", json_escape(error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, OpenOptions};
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(1);

    fn root() -> PathBuf {
        let root = env::temp_dir().join(format!(
            "cake-pinger-plan-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn executable(path: &Path, body: &str) {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(path)
            .unwrap();
        file.write_all(body.as_bytes()).unwrap();
    }

    fn settings(mode: Mode) -> Settings {
        Settings {
            section: "wan_sqm".to_string(),
            mode,
            route_mode: RouteMode::Main,
            mwan3_member: String::new(),
            configured_method: "fping".to_string(),
            configured_no_pingers: 6,
            reflector_ping_interval_s: "0.3".to_string(),
            configured_reflectors: Vec::new(),
            irtt_servers: Vec::new(),
            invalid_irtt_servers: Vec::new(),
        }
    }

    fn available() -> Availability {
        Availability {
            fping: true,
            fping_ts: true,
            tsping: true,
            irtt: true,
            ping: true,
        }
    }

    #[test]
    fn reflector_validation_is_typed_and_option_safe() {
        for value in ["1.1.1.1", "2001:db8::1", "resolver.example"] {
            assert!(valid_reflector(value), "{value}");
        }
        for value in ["-I", "999.1.1.1", "bad..name", "bad/name", "2001:::1"] {
            assert!(!valid_reflector(value), "{value}");
        }
        assert!(valid_irtt_server("[2001:db8::1]:2112"));
        assert!(!valid_irtt_server("https://example.test"));
    }

    #[test]
    fn status_uses_the_frozen_default_pool_without_probe_evidence() {
        let plan = build_plan(
            settings(Mode::Status),
            available(),
            BTreeMap::new(),
            BTreeMap::new(),
            String::new(),
        );
        assert_eq!(plan.candidate_source, "upstream-default");
        assert_eq!(plan.candidates, standard_reflectors());
        assert_eq!(plan.recommended, PingerMethod::Fping);
        assert_eq!(plan.active.len(), 6);
        assert_eq!(plan.spare.len(), 24);
        assert!(plan.encode_json().contains("\"default_pool_count\":30"));
    }

    #[test]
    fn candidate_bound_keeps_every_probe_argv_below_the_process_wall() {
        let mut configured = settings(Mode::Scan);
        configured.configured_reflectors =
            (0..80).map(|index| format!("r{index}.example")).collect();
        let plan = build_plan(
            configured,
            available(),
            BTreeMap::new(),
            BTreeMap::new(),
            String::new(),
        );
        assert_eq!(plan.candidates.len(), MAX_CANDIDATES);
        assert!(plan
            .warnings
            .iter()
            .any(|warning| warning.contains("entry limit")));
    }

    #[test]
    fn scan_selects_timestamp_responders_and_reports_rtt_fallback_pool() {
        let mut configured = settings(Mode::Scan);
        configured.configured_reflectors = vec![
            "one.example".to_string(),
            "two.example".to_string(),
            "three.example".to_string(),
        ];
        configured.configured_no_pingers = 2;
        let rtt = BTreeMap::from([
            ("one.example".to_string(), 10.0),
            ("two.example".to_string(), 20.0),
            ("three.example".to_string(), 30.0),
        ]);
        let timestamp = BTreeMap::from([
            ("one.example".to_string(), 11.0),
            ("three.example".to_string(), 31.0),
        ]);
        let plan = build_plan(
            configured,
            available(),
            rtt,
            timestamp,
            "fping-ts".to_string(),
        );
        assert_eq!(plan.recommended, PingerMethod::FpingTs);
        assert_eq!(plan.active, ["one.example", "three.example"]);
        assert!(plan.rtt_reflectors.starts_with(&[
            "one.example".to_string(),
            "two.example".to_string(),
            "three.example".to_string(),
        ]));
        assert!(plan.bad.contains(&"two.example".to_string()));
    }

    #[test]
    fn explicit_irtt_uses_only_valid_configured_servers() {
        let mut configured = settings(Mode::Status);
        configured.configured_method = "irtt".to_string();
        configured.configured_no_pingers = 3;
        configured.irtt_servers =
            vec!["irtt.example".to_string(), "[2001:db8::1]:2112".to_string()];
        configured.invalid_irtt_servers = vec!["https://bad.example".to_string()];
        let plan = build_plan(
            configured,
            available(),
            BTreeMap::new(),
            BTreeMap::new(),
            String::new(),
        );
        assert_eq!(plan.recommended, PingerMethod::Irtt);
        assert_eq!(plan.recommended_no_pingers, 2);
        assert_eq!(plan.active.len(), 2);
        assert!(plan.spare.is_empty());
        assert!(plan
            .warnings
            .iter()
            .any(|warning| warning.contains("https://bad.example")));
    }

    #[test]
    fn probe_parsers_ignore_malformed_and_duplicate_rows() {
        let fping = parse_fping(
            "one.example : xmt/rcv/%loss = 1/1/0%, min/avg/max = 1.0/2.5/3.0\n\
             bad row\n\
             one.example : xmt/rcv/%loss = 1/1/0%, min/avg/max = 5/6/7\n",
        );
        assert_eq!(fping.get("one.example"), Some(&2.5));
        let tsping =
            parse_tsping("0,one.example,a,b,c,d,e,f,1.25,2.75\n0,bad/name,a,b,c,d,e,f,1,2\n");
        assert_eq!(tsping.get("one.example"), Some(&4.0));
        assert_eq!(tsping.len(), 1);
    }

    #[test]
    fn command_status_and_scan_preserve_the_luci_json_contract() {
        let root = root();
        let uci = root.join("uci");
        executable(
            &uci,
            "#!/bin/sh\ncase \"$3\" in\n\
             *.pinger_method) echo fping ;;\n\
             *.no_pingers) echo 2 ;;\n\
             *.reflector) echo 'one.example two.example' ;;\n\
             *.route_mode) echo main ;;\n\
             *) exit 1 ;;\n\
             esac\n",
        );
        let fping = root.join("fping");
        executable(
            &fping,
            "#!/bin/sh\ncase \"$*\" in\n\
             *--help*) echo --icmp-timestamp ;;\n\
             *--icmp-timestamp*) echo 'one.example : xmt/rcv/%loss = 1/1/0%, min/avg/max = 1/2/3' >&2; echo 'two.example : xmt/rcv/%loss = 1/1/0%, min/avg/max = 1/3/4' >&2 ;;\n\
             *) echo 'one.example : xmt/rcv/%loss = 1/1/0%, min/avg/max = 1/2/3' >&2; echo 'two.example : xmt/rcv/%loss = 1/1/0%, min/avg/max = 1/3/4' >&2 ;;\n\
             esac\n",
        );
        let mut environment = Environment {
            uci,
            fping: Some(fping),
            ping: None,
            tsping: None,
            irtt: None,
            timeout: None,
            mwan3: None,
            nft: None,
            ubus: None,
            apk: None,
        };
        let status = run_with_environment(
            &mut environment,
            ["wan_sqm", "status"].into_iter().map(str::to_string),
        )
        .unwrap();
        assert!(status.contains("\"mode\":\"status\""));
        assert!(status.contains("\"recommended_method\":\"fping\""));
        let scan = run_with_environment(
            &mut environment,
            ["wan_sqm", "scan"].into_iter().map(str::to_string),
        )
        .unwrap();
        assert!(scan.contains("\"recommended_method\":\"fping-ts\""));
        assert!(scan.contains("\"timestamp_ok_count\":2"));
        assert!(scan.contains("\"host\":\"one.example\""));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mwan3_scan_requires_online_status_and_routes_the_probe_exactly() {
        let root = root();
        let log = root.join("mwan.log");
        let nft = root.join("nft");
        executable(&nft, "#!/bin/sh\nexit 0\n");
        let ubus = root.join("ubus");
        executable(&ubus, "#!/bin/sh\necho '{\"status\":\"online\"}'\n");
        let fping = root.join("fping");
        executable(
            &fping,
            "#!/bin/sh\necho 'one.example : xmt/rcv/%loss = 1/1/0%, min/avg/max = 1/2/3' >&2\n",
        );
        let mwan3 = root.join("mwan3");
        executable(
            &mwan3,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" > '{}'\n[ \"$1\" = use ] || exit 2\nshift\n[ \"$1\" = wanb ] || exit 2\nshift\n[ \"$1\" = exec ] || exit 2\nshift\nexec \"$@\"\n",
                log.display()
            ),
        );
        let environment = Environment {
            uci: PathBuf::from("/bin/false"),
            fping: Some(fping.clone()),
            ping: None,
            tsping: None,
            irtt: None,
            timeout: None,
            mwan3: Some(mwan3),
            nft: Some(nft),
            ubus: Some(ubus.clone()),
            apk: None,
        };
        let mut configured = settings(Mode::Scan);
        configured.route_mode = RouteMode::Mwan3;
        configured.mwan3_member = "wanb".to_string();
        check_route(&environment, &configured).unwrap();
        assert_eq!(
            probe_fping(
                &environment,
                &configured,
                &["one.example".to_string()],
                false,
            )
            .unwrap()
            .get("one.example"),
            Some(&2.0)
        );
        let routed = fs::read_to_string(&log).unwrap();
        assert!(routed.starts_with(&format!("use wanb exec {} ", fping.display())));

        fs::write(&ubus, "#!/bin/sh\necho '{\"status\":\"offline\"}'\n").unwrap();
        assert!(check_route(&environment, &configured)
            .unwrap_err()
            .contains("offline"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn install_backend_is_an_enum_and_never_becomes_an_apk_argument() {
        assert!(PingerMethod::parse("fping;reboot").is_err());
        assert_eq!(PingerMethod::Fping.package(), "fping");
        assert_eq!(PingerMethod::Irtt.package(), "irtt");
        assert!(!PingerMethod::Tsping.installable());
    }

    #[test]
    fn errors_remain_machine_readable_for_luci() {
        assert_eq!(
            error_json("bad \"route\""),
            "{\"error\":\"bad \\\"route\\\"\"}\n"
        );
    }
}
