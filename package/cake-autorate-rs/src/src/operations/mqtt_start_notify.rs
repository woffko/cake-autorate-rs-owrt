//! One-shot local startup notification. Datagram receipt never replaces the
//! caller's exact procd/PID/source checks. Abstract sockets leave no crash files.
//! The abstract endpoint is not an authentication boundary: all incoming bytes
//! are untrusted hints and cannot independently authorize lifecycle success.

use super::MqttPublisherConfig;
use crate::operations::identity::{read_kernel_uuid, ProcessIdentity};
use sha2::{Digest, Sha256};
use std::io;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::path::Path;
use std::time::Instant;

pub(crate) const ENV: &str = "CAKE_AUTORATE_MQTT_READY_ENDPOINT";
const PREFIX: &str = "cmq1_";
const SIZE: usize = 48;

pub(crate) struct Receipt {
    pub(crate) pid: u32,
    pub(crate) starttime: u64,
    digest: [u8; 32],
}

fn address(name: &str) -> Result<SocketAddr, String> {
    let suffix = name
        .strip_prefix(PREFIX)
        .ok_or("MQTT startup endpoint invalid")?;
    if suffix.len() != 32
        || !suffix
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("MQTT startup endpoint invalid".into());
    }
    SocketAddr::from_abstract_name(name.as_bytes())
        .map_err(|_| "MQTT startup address invalid".into())
}

pub(crate) struct Startup {
    socket: UnixDatagram,
    name: String,
}

impl Startup {
    pub(crate) fn new() -> Result<Self, String> {
        let nonce = read_kernel_uuid(
            Path::new("/proc/sys/kernel/random/uuid"),
            "MQTT startup nonce",
        )?;
        let name = format!("{PREFIX}{nonce}");
        let socket = UnixDatagram::bind_addr(&address(&name)?)
            .map_err(|_| "MQTT startup socket unavailable")?;
        Ok(Self { socket, name })
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn wait(
        &self,
        digest: [u8; 32],
        deadline: Instant,
        mut observe: impl FnMut() -> Result<Option<(u32, u64)>, String>,
    ) -> Result<Receipt, String> {
        let mut received: Option<Receipt> = None;
        for _ in 0..32 {
            let current = observe()?;
            if let Some(receipt) = &received {
                if current == Some((receipt.pid, receipt.starttime)) {
                    return Ok(received.take().unwrap());
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("MQTT startup deadline expired".into());
            }
            self.socket
                .set_read_timeout(Some(remaining))
                .map_err(|_| "MQTT startup deadline setup failed")?;
            let mut bytes = [0u8; SIZE + 1];
            match self.socket.recv(&mut bytes) {
                Ok(SIZE) if bytes[..4] == *b"CMQ1" => {
                    let pid = u32::from_be_bytes(
                        bytes[4..8]
                            .try_into()
                            .map_err(|_| "MQTT startup PID invalid")?,
                    );
                    let starttime = u64::from_be_bytes(
                        bytes[8..16]
                            .try_into()
                            .map_err(|_| "MQTT startup identity invalid")?,
                    );
                    let observed: [u8; 32] = bytes[16..48]
                        .try_into()
                        .map_err(|_| "MQTT startup plan invalid")?;
                    if pid <= 1 || starttime == 0 || observed != digest {
                        return Err("MQTT startup receipt identity or plan mismatch".into());
                    }
                    received = Some(Receipt {
                        pid,
                        starttime,
                        digest: observed,
                    });
                }
                Ok(_) => return Err("MQTT startup receipt malformed".into()),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    // Timeout supplies no evidence. Only a previously received
                    // startup receipt plus a fresh exact observation may pass.
                    if let Some(receipt) = received {
                        if observe()? == Some((receipt.pid, receipt.starttime)) {
                            return Ok(receipt);
                        }
                    }
                    return Err("MQTT startup deadline expired".into());
                }
                Err(_) => return Err("MQTT startup receive failed".into()),
            }
        }
        Err("MQTT startup event bound exceeded".into())
    }
}

pub(crate) fn send(
    name: &str,
    config: &MqttPublisherConfig,
    process: &ProcessIdentity,
) -> Result<(), String> {
    let digest: [u8; 32] = Sha256::digest(config.encode_plan()?).into();
    let receipt = Receipt {
        pid: process.pid,
        starttime: process.starttime_ticks,
        digest,
    };
    let mut bytes = [0u8; SIZE];
    bytes[..4].copy_from_slice(b"CMQ1");
    bytes[4..8].copy_from_slice(&receipt.pid.to_be_bytes());
    bytes[8..16].copy_from_slice(&receipt.starttime.to_be_bytes());
    bytes[16..].copy_from_slice(&receipt.digest);
    let socket = UnixDatagram::unbound().map_err(|_| "MQTT startup sender unavailable")?;
    socket
        .set_nonblocking(true)
        .map_err(|_| "MQTT startup sender setup failed")?;
    socket
        .connect_addr(&address(name)?)
        .map_err(|_| "MQTT startup receiver unavailable")?;
    if socket
        .send(&bytes)
        .map_err(|_| "MQTT startup notification failed")?
        != SIZE
    {
        return Err("MQTT startup notification incomplete".into());
    }
    Ok(())
}

pub(super) fn notify(config: &MqttPublisherConfig) {
    let Ok(name) = std::env::var(ENV) else {
        return;
    };
    let Ok(process) = ProcessIdentity::current() else {
        return;
    };
    // A later procd respawn retains the old optional endpoint. Its disappearance
    // must not prevent the publisher from operating after the parent has exited.
    let _ = send(&name, config, &process);
}

#[cfg(test)]
mod tests {
    use super::super::tests::section;
    use super::*;
    use std::time::Duration;

    #[test]
    fn r4_mqtt_start_receipt_is_bound_to_plan_pid_and_process_generation() {
        let startup = Startup::new().unwrap();
        let config = MqttPublisherConfig::from_section("lab", &section(&[]))
            .unwrap()
            .unwrap();
        let process = ProcessIdentity::current().unwrap();
        let digest = Sha256::digest(config.encode_plan().unwrap()).into();
        send(startup.name(), &config, &process).unwrap();
        let mut observations = 0;
        let receipt = startup
            .wait(digest, Instant::now() + Duration::from_secs(1), || {
                observations += 1;
                Ok(Some((process.pid, process.starttime_ticks)))
            })
            .unwrap();
        assert_eq!(receipt.pid, process.pid);
        assert_eq!(
            observations, 2,
            "live PID without a notification is insufficient"
        );
        send(startup.name(), &config, &process).unwrap();
        assert!(startup
            .wait(digest, Instant::now() + Duration::from_millis(2), || Ok(
                Some((process.pid, process.starttime_ticks + 1))
            ))
            .is_err());
        send(startup.name(), &config, &process).unwrap();
        assert!(startup
            .wait([0u8; 32], Instant::now() + Duration::from_secs(1), || Ok(
                None
            ))
            .is_err());
        send(startup.name(), &config, &process).unwrap();
        assert_eq!(
            startup
                .wait(digest, Instant::now() + Duration::from_secs(1), || Err(
                    "source-changed".into()
                ))
                .err()
                .unwrap(),
            "source-changed"
        );
    }

    #[test]
    fn r4_mqtt_start_socket_disappears_on_drop_and_rejects_invalid_endpoints() {
        let startup = Startup::new().unwrap();
        let name = startup.name().to_string();
        let config = MqttPublisherConfig::from_section("lab", &section(&[]))
            .unwrap()
            .unwrap();
        let process = ProcessIdentity::current().unwrap();
        drop(startup);
        assert!(send(&name, &config, &process).is_err());
        for name in ["", "../socket", "cmq1_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"] {
            assert!(address(name).is_err());
        }
    }

    #[test]
    fn r4_mqtt_start_waits_for_delayed_receipt_and_never_accepts_pid_alone() {
        let startup = Startup::new().unwrap();
        let config = MqttPublisherConfig::from_section("lab", &section(&[]))
            .unwrap()
            .unwrap();
        let process = ProcessIdentity::current().unwrap();
        let digest = Sha256::digest(config.encode_plan().unwrap()).into();
        assert!(startup
            .wait(digest, Instant::now() + Duration::from_millis(2), || Ok(
                Some((process.pid, process.starttime_ticks))
            ))
            .is_err());
        let (begin, received) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let sender = (startup.name().to_string(), config.clone(), process.clone());
            scope.spawn(move || {
                received.recv().unwrap();
                // Only the fixture delays delivery; production blocks on the
                // socket with one deadline and contains no sleep/poll cadence.
                std::thread::sleep(Duration::from_millis(5));
                send(&sender.0, &sender.1, &sender.2).unwrap();
            });
            let mut begin = Some(begin);
            let receipt = startup
                .wait(digest, Instant::now() + Duration::from_secs(1), || {
                    if let Some(begin) = begin.take() {
                        begin.send(()).unwrap();
                    }
                    Ok(Some((process.pid, process.starttime_ticks)))
                })
                .unwrap();
            assert_eq!(receipt.starttime, process.starttime_ticks);
        });
    }
}
