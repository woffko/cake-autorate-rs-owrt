//! Tests for the TLS transport of the MQTT publisher: the upgrade invariant,
//! the plan encodings, the CA file rules and a TLS broker in this process.

use super::tests::{section, temp_root};
use super::tls::test_support::TrustedOwner;
use super::*;
use rcgen::{
    date_time_ymd, BasicConstraints, Certificate, CertificateParams, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose, SanType,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use std::net::{IpAddr, Ipv4Addr, TcpListener};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

/// The version-1 plans of the release before TLS existed, produced by that
/// release's encoder from the fixtures below. A configuration without
/// `mqtt_tls` must keep encoding to exactly these bytes.
const GOLDEN_PLAIN_PLAN: &str = "63616b652d6175746f726174652d6d7174742d706c616e00010000000377616e0000000e62726f6b65722e696e76616c696400000000000000000000000d686f6d65617373697374616e740000000d63616b652d6175746f726174650000001163616b655f6175746f726174655f77616e0000001363616b652d6175746f72617465202877616e29000000082f7661722f6c6f67075b000000000000000100";
const GOLDEN_FULL_PLAN: &str = "63616b652d6175746f726174652d6d7174742d706c616e00010000000577616e5f620000000e62726f6b65722e696e76616c6964000000097075626c69736865720000001270726976617465207061737370687261736500000002686100000006726f757465720000000867775f77616e5f620000000f47617465776179202877616e5f6229000000092f746d702f6c6f6773075c000000000000001e01";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn from_section(extra: &[(&str, &str)]) -> Result<MqttPublisherConfig, String> {
    MqttPublisherConfig::from_section("wan", &section(extra)).map(|config| config.unwrap())
}

fn full_options() -> Vec<(&'static str, &'static str)> {
    vec![
        ("mqtt_username", "publisher"),
        ("mqtt_password", "private passphrase"),
        ("mqtt_port", "1884"),
        ("mqtt_min_interval_s", "30"),
        ("mqtt_publish_cpu_stats", "1"),
        ("output_cpu_stats", "1"),
        ("mqtt_discovery_prefix", "ha"),
        ("mqtt_base_topic", "router"),
        ("mqtt_device_id", "gw"),
        ("mqtt_device_name", "Gateway"),
        ("log_file_path_override", "/tmp/logs"),
    ]
}

#[test]
fn a_configuration_without_mqtt_tls_stays_plain_mqtt_after_an_upgrade() {
    let config = from_section(&[]).unwrap();
    assert!(!config.tls);
    assert!(config.tls_verify);
    assert!(config.tls_ca_file.is_empty());
    assert_eq!(config.port, 1883);
    assert_eq!(
        from_section(&[("mqtt_tls", "0")]).unwrap().port,
        1883,
        "an explicit 0 is the same plain broker"
    );
    // The same section as saved by the release before TLS existed.
    assert_eq!(
        from_section(&[("mqtt_port", "1884")]).unwrap().port,
        1884,
        "an explicit port always wins"
    );
}

#[test]
fn tls_without_a_port_uses_8883_and_an_explicit_port_wins() {
    let config = from_section(&[("mqtt_tls", "1")]).unwrap();
    assert!(config.tls);
    assert!(config.tls_verify);
    assert!(config.tls_ca_file.is_empty());
    assert_eq!(config.port, 8883);
    assert_eq!(
        from_section(&[("mqtt_tls", "1"), ("mqtt_port", "1883")])
            .unwrap()
            .port,
        1883
    );
    assert!(from_section(&[("mqtt_tls", "1"), ("mqtt_port", "0")]).is_err());
    assert!(from_section(&[("mqtt_tls", "yes")]).is_err());
    assert!(from_section(&[("mqtt_tls", "1"), ("mqtt_tls_verify", "2")]).is_err());
}

#[test]
fn plain_plans_keep_the_bytes_of_the_release_before_tls() {
    let plain = from_section(&[]).unwrap();
    assert_eq!(hex(&plain.encode_plan().unwrap()), GOLDEN_PLAIN_PLAN);
    let mut options = full_options();
    let full = MqttPublisherConfig::from_section("wan_b", &section(&options))
        .unwrap()
        .unwrap();
    assert_eq!(hex(&full.encode_plan().unwrap()), GOLDEN_FULL_PLAN);
    // TLS settings that cannot apply do not touch a plain plan, and an
    // explicit `mqtt_tls=0` is the same plan as no option at all.
    options.extend([
        ("mqtt_tls", "0"),
        ("mqtt_tls_verify", "0"),
        ("mqtt_tls_ca_file", "/etc/ssl/ignored.pem"),
    ]);
    let ignored = MqttPublisherConfig::from_section("wan_b", &section(&options))
        .unwrap()
        .unwrap();
    assert_eq!(ignored, full);
    assert_eq!(hex(&ignored.encode_plan().unwrap()), GOLDEN_FULL_PLAN);
    // The old plan files decode to the plain configuration.
    let golden = (0..GOLDEN_PLAIN_PLAN.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&GOLDEN_PLAIN_PLAN[at..at + 2], 16).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(MqttPublisherConfig::decode_plan(&golden).unwrap(), plain);
}

#[test]
fn a_plan_file_of_the_release_before_tls_is_unchanged_after_the_upgrade() {
    let root = temp_root("tls-upgrade");
    let golden = (0..GOLDEN_PLAIN_PLAN.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&GOLDEN_PLAIN_PLAN[at..at + 2], 16).unwrap())
        .collect::<Vec<_>>();
    // The old release left this file in the plan root.
    publish_plan_files(&root, &[("wan".to_string(), golden.clone())]).unwrap();
    let mut package = UciPackage::default();
    package.sections.insert("wan".to_string(), section(&[]));
    // The upgraded daemon reads the same section: nothing changed, so the
    // running publisher is not restarted and the file is not rewritten.
    assert_eq!(attest_service_plans(&package, &root).unwrap(), ["wan"]);
    assert_eq!(fs::read(root.join("wan.plan")).unwrap(), golden);
    assert_eq!(read_service_plan(&root, "wan").unwrap().port, 1883);
    // Publishing the plans again writes the very same bytes.
    publish_service_plans(&package, &root).unwrap();
    assert_eq!(fs::read(root.join("wan.plan")).unwrap(), golden);
    // Turning TLS on is a change that restarts the publisher.
    package
        .sections
        .get_mut("wan")
        .unwrap()
        .options
        .insert("mqtt_tls".to_string(), "1".to_string());
    assert!(attest_service_plans(&package, &root).is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn tls_plans_use_version_two_and_round_trip() {
    let trusted = from_section(&[
        ("mqtt_tls", "1"),
        ("mqtt_tls_ca_file", "/etc/ssl/broker-ca.pem"),
    ])
    .unwrap();
    let plan = trusted.encode_plan().unwrap();
    assert!(plan.starts_with(PLAN_MAGIC_TLS));
    assert_ne!(plan, from_section(&[]).unwrap().encode_plan().unwrap());
    assert_eq!(MqttPublisherConfig::decode_plan(&plan).unwrap(), trusted);

    let unverified = from_section(&[
        ("mqtt_tls", "1"),
        ("mqtt_tls_verify", "0"),
        ("mqtt_tls_ca_file", "/etc/ssl/not-used.pem"),
    ])
    .unwrap();
    assert!(!unverified.tls_verify);
    assert!(
        unverified.tls_ca_file.is_empty(),
        "a CA file is meaningless without verification"
    );
    let plan = unverified.encode_plan().unwrap();
    assert_eq!(MqttPublisherConfig::decode_plan(&plan).unwrap(), unverified);
    assert_ne!(
        plan,
        from_section(&[("mqtt_tls", "1")])
            .unwrap()
            .encode_plan()
            .unwrap()
    );
}

#[test]
fn a_tls_setting_changes_the_plan_that_restarts_the_publisher() {
    let root = temp_root("tls-plans");
    let mut package = UciPackage::default();
    package
        .sections
        .insert("wan".to_string(), section(&[("mqtt_port", "1883")]));
    publish_service_plans(&package, &root).unwrap();
    assert_eq!(attest_service_plans(&package, &root).unwrap(), ["wan"]);
    for change in [
        ("mqtt_tls", "1"),
        ("mqtt_tls", "0"), // back to plain: the original plan again
    ] {
        package
            .sections
            .get_mut("wan")
            .unwrap()
            .options
            .insert(change.0.to_string(), change.1.to_string());
        if change.1 == "1" {
            assert!(attest_service_plans(&package, &root).is_err());
            publish_service_plans(&package, &root).unwrap();
        } else {
            assert!(attest_service_plans(&package, &root).is_err());
            publish_service_plans(&package, &root).unwrap();
            assert_eq!(attest_service_plans(&package, &root).unwrap(), ["wan"]);
        }
    }
    // Verification and the CA file are part of the TLS plan.
    for (name, value) in [("mqtt_tls", "1"), ("mqtt_tls_ca_file", "/etc/ssl/a.pem")] {
        package
            .sections
            .get_mut("wan")
            .unwrap()
            .options
            .insert(name.to_string(), value.to_string());
    }
    publish_service_plans(&package, &root).unwrap();
    package
        .sections
        .get_mut("wan")
        .unwrap()
        .options
        .insert("mqtt_tls_ca_file".to_string(), "/etc/ssl/b.pem".to_string());
    assert!(attest_service_plans(&package, &root).is_err());
    package
        .sections
        .get_mut("wan")
        .unwrap()
        .options
        .insert("mqtt_tls_verify".to_string(), "0".to_string());
    assert!(attest_service_plans(&package, &root).is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn plans_that_are_not_canonical_for_their_version_are_rejected() {
    let trusted = from_section(&[("mqtt_tls", "1")]).unwrap();
    let plan = trusted.encode_plan().unwrap();

    // A version-1 header on TLS bytes, and a version-2 header on a plain
    // broker, are two spellings of one configuration.
    let mut v1_header = plan.clone();
    v1_header[PLAN_MAGIC.len() - 1] = 1;
    assert!(MqttPublisherConfig::decode_plan(&v1_header).is_err());
    let mut v2_header = from_section(&[]).unwrap().encode_plan().unwrap();
    v2_header[PLAN_MAGIC.len() - 1] = 2;
    assert!(MqttPublisherConfig::decode_plan(&v2_header).is_err());

    // Trailing data, a bad boolean and an unknown version.
    let mut trailing = plan.clone();
    trailing.push(0);
    assert!(MqttPublisherConfig::decode_plan(&trailing).is_err());
    let mut bad_boolean = plan.clone();
    let verify_at = plan.len() - 5; // the verify byte precedes the empty CA text
    assert_eq!(bad_boolean[verify_at], 1);
    bad_boolean[verify_at] = 2;
    assert!(MqttPublisherConfig::decode_plan(&bad_boolean).is_err());
    let mut unknown = plan;
    unknown[PLAN_MAGIC.len() - 1] = 3;
    assert!(MqttPublisherConfig::decode_plan(&unknown).is_err());
}

#[test]
fn the_ca_path_is_validated_when_the_plan_is_built() {
    for path in ["relative.pem", "/etc/ssl/dir/", "/etc/ssl/a\nb.pem"] {
        assert!(
            from_section(&[("mqtt_tls", "1"), ("mqtt_tls_ca_file", path)]).is_err(),
            "{path:?} must be refused"
        );
    }
    let long = format!("/{}", "a".repeat(1024));
    assert!(from_section(&[("mqtt_tls", "1"), ("mqtt_tls_ca_file", &long)]).is_err());
    let longest = format!("/{}", "a".repeat(1023));
    assert!(from_section(&[("mqtt_tls", "1"), ("mqtt_tls_ca_file", &longest)]).is_ok());
    // Without TLS the option is not looked at.
    assert!(from_section(&[("mqtt_tls_ca_file", "relative.pem")]).is_ok());
}

// --- certificates -----------------------------------------------------------

/// (year, month, day) of the first and the last day a certificate is valid.
type Validity = ((i32, u8, u8), (i32, u8, u8));

struct Authority {
    certificate: Certificate,
    key: KeyPair,
}

impl Authority {
    fn new(name: &str) -> Self {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(Vec::new()).unwrap();
        params.distinguished_name = DistinguishedName::new();
        params.distinguished_name.push(DnType::CommonName, name);
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let certificate = params.self_signed(&key).unwrap();
        Self { certificate, key }
    }

    fn pem(&self) -> String {
        self.certificate.pem()
    }

    /// A server certificate for `names` (and 127.0.0.1) signed by this
    /// authority; `valid` is an optional (not before, not after) pair of dates.
    fn server(&self, names: &[&str], valid: Option<Validity>) -> ServerIdentity {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(
            names
                .iter()
                .map(|name| name.to_string())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        params
            .subject_alt_names
            .push(SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        if let Some((from, until)) = valid {
            params.not_before = date_time_ymd(from.0, from.1, from.2);
            params.not_after = date_time_ymd(until.0, until.1, until.2);
        }
        let certificate = params
            .signed_by(&key, &self.certificate, &self.key)
            .unwrap();
        ServerIdentity {
            chain: vec![certificate.der().clone()],
            key: PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
        }
    }
}

struct ServerIdentity {
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
}

impl ServerIdentity {
    fn config(self) -> Arc<ServerConfig> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        Arc::new(
            ServerConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(self.chain, self.key)
                .unwrap(),
        )
    }
}

// --- the CA file --------------------------------------------------------------

fn write_file(root: &Path, name: &str, contents: &[u8], mode: u32) -> String {
    let path = root.join(name);
    fs::write(&path, contents).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    path.to_str().unwrap().to_string()
}

fn owner_of(path: &str) -> u32 {
    fs::metadata(path).unwrap().uid()
}

#[test]
fn the_ca_file_must_be_a_safe_pem_file_with_certificates() {
    let root = temp_root("tls-ca");
    let authority = Authority::new("Test CA");
    let pem = authority.pem();
    let good = write_file(&root, "ca.pem", pem.as_bytes(), 0o644);
    let _owner = TrustedOwner::set(owner_of(&good));
    assert_eq!(tls::read_ca_certificates(&good).unwrap().len(), 1);
    assert!(tls::check_ca_file(&good).is_ok());
    assert!(tls::client_config(true, &good).is_ok());

    let two = format!("{pem}\n{}", Authority::new("Second CA").pem());
    let both = write_file(&root, "both.pem", two.as_bytes(), 0o600);
    assert_eq!(tls::read_ca_certificates(&both).unwrap().len(), 2);

    let refused = |path: &str, message: &str| {
        let error = tls::read_ca_certificates(path).unwrap_err();
        assert!(error.contains(message), "{path}: {error}");
        assert!(tls::client_config(true, path).is_err());
        assert!(tls::check_ca_file(path).is_err());
    };
    refused("relative.pem", "absolute");
    refused(&format!("/{}", "a".repeat(1100)), "absolute");
    refused(
        &root.join("missing.pem").display().to_string(),
        "unable to open",
    );
    refused(&root.display().to_string(), "not a regular file");
    refused(
        &write_file(&root, "group.pem", pem.as_bytes(), 0o664),
        "writable by group or others",
    );
    refused(
        &write_file(&root, "other.pem", pem.as_bytes(), 0o646),
        "writable by group or others",
    );
    refused(&write_file(&root, "empty.pem", b"", 0o600), "empty");
    refused(
        &write_file(&root, "large.pem", &vec![b'A'; 300 * 1024], 0o600),
        "256 KiB",
    );
    refused(
        &write_file(&root, "text.pem", b"not a certificate\n", 0o600),
        "no certificate",
    );
    refused(
        &write_file(
            &root,
            "key.pem",
            authority.key.serialize_pem().as_bytes(),
            0o600,
        ),
        "no certificate",
    );
    refused(
        &write_file(
            &root,
            "broken.pem",
            b"-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----\n",
            0o600,
        ),
        "not valid PEM",
    );
    let link = root.join("link.pem");
    std::os::unix::fs::symlink(&good, &link).unwrap();
    refused(&link.display().to_string(), "symbolic link");
    drop(_owner);

    // Only root's files are trusted: the same file, owned by someone else.
    let _other = TrustedOwner::set(owner_of(&good).wrapping_add(1));
    refused(&good, "owned by root");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn built_in_roots_and_unverified_mode_need_no_file() {
    assert!(tls::client_config(true, "").is_ok());
    assert!(tls::client_config(false, "").is_ok());
    // Without verification a CA file is never opened.
    assert!(tls::client_config(false, "/nonexistent/ca.pem").is_ok());
}

// --- a TLS broker in this process -----------------------------------------------

type ServerStream = StreamOwned<ServerConnection, TcpStream>;

/// Accept one connection, run the TLS handshake and hand the stream to
/// `session`. A handshake the client refuses ends the thread with an error.
fn spawn_tls_broker<F>(
    server: Arc<ServerConfig>,
    session: F,
) -> (u16, JoinHandle<Result<(), String>>)
where
    F: FnOnce(&mut ServerStream) -> Result<(), String> + Send + 'static,
{
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let thread = thread::spawn(move || {
        let (mut socket, _) = listener.accept().map_err(|error| error.to_string())?;
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|error| error.to_string())?;
        socket
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(|error| error.to_string())?;
        let mut connection = ServerConnection::new(server).map_err(|error| error.to_string())?;
        while connection.is_handshaking() {
            connection
                .complete_io(&mut socket)
                .map_err(|error| format!("server handshake: {error}"))?;
        }
        session(&mut StreamOwned::new(connection, socket))
    });
    (port, thread)
}

fn accept_connect<S: Read + Write>(stream: &mut S) -> Result<Vec<u8>, String> {
    let (header, connect) = read_packet(stream)?;
    if header != 0x10 || !connect.windows(7).any(|window| window == b"offline") {
        return Err("broker expected a CONNECT with a will".to_string());
    }
    stream
        .write_all(&[0x20, 0x02, 0x00, 0x00])
        .map_err(|error| error.to_string())?;
    Ok(connect)
}

/// The conversation of `native_client_completes_connect_publish_ping_and_disconnect`.
fn serve_a_publisher<S: Read + Write>(stream: &mut S) -> Result<(), String> {
    accept_connect(stream)?;
    let (header, publish) = read_packet(stream)?;
    if header != 0x33 {
        return Err(format!("expected a retained PUBLISH, got {header:#x}"));
    }
    let topic_len = usize::from(u16::from_be_bytes([publish[0], publish[1]]));
    let id = [publish[2 + topic_len], publish[3 + topic_len]];
    stream
        .write_all(&[0x40, 0x02, id[0], id[1]])
        .map_err(|error| error.to_string())?;
    let (header, body) = read_packet(stream)?;
    if (header, body.as_slice()) != (0xc0, &[][..]) {
        return Err("expected a PINGREQ".to_string());
    }
    stream
        .write_all(&[0xd0, 0x00])
        .map_err(|error| error.to_string())?;
    let (header, body) = read_packet(stream)?;
    if (header, body.as_slice()) != (0xe0, &[][..]) {
        return Err("expected a DISCONNECT".to_string());
    }
    Ok(())
}

fn tls_config(host: &str, port: u16, extra: &[(&str, &str)]) -> MqttPublisherConfig {
    let mut options = vec![("mqtt_tls", "1")];
    options.extend_from_slice(extra);
    let mut config = from_section(&options).unwrap();
    config.host = host.to_string();
    config.port = port;
    config
}

fn run_publisher_conversation(config: &MqttPublisherConfig) {
    let mut client = MqttClient::connect(config).unwrap();
    assert!(client.tls_summary(config.tls_verify).is_some());
    client
        .publish(&config.availability_topic(), b"online", true)
        .unwrap();
    client.ping().unwrap();
    client.disconnect().unwrap();
}

struct Fixture {
    root: PathBuf,
    ca_file: String,
    authority: Authority,
    _owner: TrustedOwner,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = temp_root(label);
        let authority = Authority::new("Test CA");
        let ca_file = write_file(&root, "ca.pem", authority.pem().as_bytes(), 0o600);
        let owner = TrustedOwner::set(owner_of(&ca_file));
        Self {
            root,
            ca_file,
            authority,
            _owner: owner,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_tls_broker_with_a_private_ca_gets_the_full_conversation() {
    let fixture = Fixture::new("tls-session");
    for host in ["localhost", "127.0.0.1"] {
        let identity = fixture.authority.server(&["localhost"], None);
        let (port, broker) = spawn_tls_broker(identity.config(), serve_a_publisher);
        let config = tls_config(host, port, &[("mqtt_tls_ca_file", &fixture.ca_file)]);
        run_publisher_conversation(&config);
        broker.join().unwrap().unwrap();
    }
}

#[test]
fn a_broker_that_the_configured_trust_does_not_cover_is_refused() {
    let fixture = Fixture::new("tls-untrusted");
    let other = Authority::new("Another CA");
    let other_file = write_file(&fixture.root, "other.pem", other.pem().as_bytes(), 0o600);
    let identity = || fixture.authority.server(&["localhost"], None);

    // The CA file replaces the built-in roots: a different CA does not match.
    let (port, broker) = spawn_tls_broker(identity().config(), serve_a_publisher);
    let error = MqttClient::connect(&tls_config(
        "localhost",
        port,
        &[("mqtt_tls_ca_file", &other_file)],
    ))
    .err()
    .unwrap();
    assert!(error.contains("MQTT TLS handshake failed"), "{error}");
    assert!(error.contains("UnknownIssuer"), "{error}");
    assert!(broker.join().unwrap().is_err());

    // With no CA file only the built-in public roots count, not this private CA.
    let (port, broker) = spawn_tls_broker(identity().config(), serve_a_publisher);
    let error = MqttClient::connect(&tls_config("localhost", port, &[]))
        .err()
        .unwrap();
    assert!(error.contains("UnknownIssuer"), "{error}");
    assert!(broker.join().unwrap().is_err());
}

#[test]
fn an_expired_certificate_is_refused() {
    let fixture = Fixture::new("tls-expired");
    let identity = fixture
        .authority
        .server(&["localhost"], Some(((2000, 1, 1), (2001, 1, 1))));
    let (port, broker) = spawn_tls_broker(identity.config(), serve_a_publisher);
    let error = MqttClient::connect(&tls_config(
        "localhost",
        port,
        &[("mqtt_tls_ca_file", &fixture.ca_file)],
    ))
    .err()
    .unwrap();
    assert!(error.contains("certificate expired"), "{error}");
    assert!(broker.join().unwrap().is_err());
}

#[test]
fn a_certificate_that_is_not_valid_yet_is_refused_and_says_so() {
    // A router whose clock is behind sees a newly issued certificate this way.
    let fixture = Fixture::new("tls-not-yet");
    let identity = fixture
        .authority
        .server(&["localhost"], Some(((2090, 1, 1), (2091, 1, 1))));
    let (port, broker) = spawn_tls_broker(identity.config(), serve_a_publisher);
    let error = MqttClient::connect(&tls_config(
        "localhost",
        port,
        &[("mqtt_tls_ca_file", &fixture.ca_file)],
    ))
    .err()
    .unwrap();
    assert!(error.contains("not valid yet"), "{error}");
    assert!(broker.join().unwrap().is_err());
}

#[test]
fn a_certificate_for_another_name_is_refused() {
    let fixture = Fixture::new("tls-name");
    let identity = fixture.authority.server(&["other.example"], None);
    let (port, broker) = spawn_tls_broker(identity.config(), serve_a_publisher);
    let error = MqttClient::connect(&tls_config(
        "localhost",
        port,
        &[("mqtt_tls_ca_file", &fixture.ca_file)],
    ))
    .err()
    .unwrap();
    assert!(error.contains("not valid for name"), "{error}");
    assert!(broker.join().unwrap().is_err());
}

#[test]
fn switching_verification_off_accepts_a_broker_nobody_vouches_for() {
    let fixture = Fixture::new("tls-unverified");
    // Unknown issuer, wrong name and an expired certificate all pass; the
    // session is still encrypted and the summary says it is not verified.
    for (names, valid) in [
        (vec!["localhost"], None),
        (vec!["other.example"], None),
        (vec!["localhost"], Some(((2000, 1, 1), (2001, 1, 1)))),
    ] {
        let identity = Authority::new("Unknown CA").server(&names, valid);
        let (port, broker) = spawn_tls_broker(identity.config(), serve_a_publisher);
        let config = tls_config("localhost", port, &[("mqtt_tls_verify", "0")]);
        assert!(!config.tls_verify);
        let mut client = MqttClient::connect(&config).unwrap();
        assert!(client.tls_summary(false).unwrap().contains("NOT verified"));
        client
            .publish(&config.availability_topic(), b"online", true)
            .unwrap();
        client.ping().unwrap();
        client.disconnect().unwrap();
        broker.join().unwrap().unwrap();
    }
    drop(fixture);
}

#[test]
fn a_missing_ca_file_fails_before_any_connection() {
    let fixture = Fixture::new("tls-missing-ca");
    let missing = fixture.root.join("missing.pem").display().to_string();
    // Nothing listens on this port; the error must be about the file anyway.
    let error = MqttClient::connect(&tls_config(
        "localhost",
        9,
        &[("mqtt_tls_ca_file", &missing)],
    ))
    .err()
    .unwrap();
    assert!(error.contains("unable to open the MQTT CA file"), "{error}");
}

#[test]
fn a_tls_client_does_not_hang_on_a_broker_that_is_not_tls() {
    // A plain MQTT broker answers the ClientHello with nothing useful.
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut hello = [0u8; 16];
        let _ = stream.read(&mut hello);
        // A plain broker would try to parse the ClientHello as MQTT and close.
    });
    let fixture = Fixture::new("tls-vs-plain");
    let error = MqttClient::connect(&tls_config(
        "127.0.0.1",
        port,
        &[("mqtt_tls_ca_file", &fixture.ca_file)],
    ))
    .err()
    .unwrap();
    assert!(error.contains("MQTT TLS handshake failed"), "{error}");
    broker.join().unwrap();
}

#[test]
fn a_silent_broker_cannot_hold_the_tls_handshake_open() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (done_sender, done) = std::sync::mpsc::channel::<()>();
    let broker = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        // Hold the connection open without a byte until the client gave up.
        let _ = done.recv_timeout(Duration::from_secs(30));
        drop(stream);
    });
    let fixture = Fixture::new("tls-silent");
    let started = Instant::now();
    let error = MqttClient::connect(&tls_config(
        "127.0.0.1",
        port,
        &[("mqtt_tls_ca_file", &fixture.ca_file)],
    ))
    .err()
    .unwrap();
    assert!(error.contains("timed out"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(20));
    done_sender.send(()).unwrap();
    broker.join().unwrap();
}

#[test]
fn the_status_probe_completes_the_handshake_and_sends_no_mqtt_packet() {
    let fixture = Fixture::new("tls-probe");
    let identity = fixture.authority.server(&["localhost"], None);
    let (port, broker) = spawn_tls_broker(identity.config(), |stream| {
        // Only the end of the TLS session may arrive: no CONNECT, so neither
        // the user name nor the password.
        let mut seen = Vec::new();
        stream
            .read_to_end(&mut seen)
            .map_err(|error| error.to_string())?;
        if seen.is_empty() {
            Ok(())
        } else {
            Err(format!("the probe sent {} bytes of MQTT", seen.len()))
        }
    });
    let config = tls_config(
        "localhost",
        port,
        &[
            ("mqtt_tls_ca_file", &fixture.ca_file),
            ("mqtt_username", "probe-user"),
            ("mqtt_password", "probe-secret"),
        ],
    );
    let summary = config.check_tls_handshake().unwrap();
    assert!(summary.contains("server certificate verified"), "{summary}");
    broker.join().unwrap().unwrap();

    // A certificate the configured trust does not cover is reported, not hidden.
    let other = Authority::new("Another CA");
    let (port, broker) = spawn_tls_broker(
        other.server(&["localhost"], None).config(),
        serve_a_publisher,
    );
    let error = tls_config("localhost", port, &[("mqtt_tls_ca_file", &fixture.ca_file)])
        .check_tls_handshake()
        .unwrap_err();
    assert!(error.contains("UnknownIssuer"), "{error}");
    assert!(broker.join().unwrap().is_err());
}

fn socket_has_data(client: &MqttClient) -> bool {
    let mut descriptor = libc::pollfd {
        fd: client.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `descriptor` is one valid pollfd that lives for the whole call.
    let ready = unsafe { libc::poll(&mut descriptor, 1, 0) };
    ready > 0 && descriptor.revents & libc::POLLIN != 0
}

#[test]
fn packets_that_share_one_tls_record_are_not_left_waiting() {
    let fixture = Fixture::new("tls-record");
    let identity = fixture.authority.server(&["localhost"], None);
    let (port, broker) = spawn_tls_broker(identity.config(), |stream| {
        accept_connect(stream)?;
        // A PINGRESP and a DISCONNECT in one write: one TLS record.
        stream
            .write_all(&[0xd0, 0x00, 0xe0, 0x00])
            .map_err(|error| error.to_string())?;
        let mut sink = [0u8; 16];
        let _ = stream.read(&mut sink); // until the client hangs up
        Ok(())
    });
    let config = tls_config("localhost", port, &[("mqtt_tls_ca_file", &fixture.ca_file)]);
    let mut client = MqttClient::connect(&config).unwrap();
    client.read_unsolicited().unwrap(); // the PINGRESP
                                        // The DISCONNECT is decrypted and waiting, but the socket is silent: a
                                        // loop that only polled the socket would sleep on it until some other
                                        // event.
    assert!(client.has_buffered_input());
    assert!(!socket_has_data(&client));
    let error = client.read_ready_packets().unwrap_err();
    assert_eq!(error, "MQTT broker disconnected");
    assert!(!client.has_buffered_input());
    drop(client);
    broker.join().unwrap().unwrap();
}

#[test]
fn one_wakeup_reads_every_packet_the_record_carried() {
    let fixture = Fixture::new("tls-drain");
    let identity = fixture.authority.server(&["localhost"], None);
    let (port, broker) = spawn_tls_broker(identity.config(), |stream| {
        accept_connect(stream)?;
        stream
            .write_all(&[0xd0, 0x00, 0xd0, 0x00, 0xd0, 0x00, 0xe0, 0x00])
            .map_err(|error| error.to_string())?;
        let mut sink = [0u8; 16];
        let _ = stream.read(&mut sink);
        Ok(())
    });
    let config = tls_config("localhost", port, &[("mqtt_tls_ca_file", &fixture.ca_file)]);
    let mut client = MqttClient::connect(&config).unwrap();
    // Three PINGRESPs are consumed and the DISCONNECT behind them is reported
    // by the same call.
    assert_eq!(
        client.read_ready_packets().unwrap_err(),
        "MQTT broker disconnected"
    );
    drop(client);
    broker.join().unwrap().unwrap();
}

#[test]
fn plain_mqtt_has_nothing_buffered_and_reads_one_packet_per_wakeup() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        accept_connect(&mut stream).unwrap();
        stream.write_all(&[0xd0, 0x00]).unwrap();
        let mut sink = [0u8; 16];
        let _ = stream.read(&mut sink);
    });
    let mut config = from_section(&[]).unwrap();
    config.host = "127.0.0.1".to_string();
    config.port = port;
    let mut client = MqttClient::connect(&config).unwrap();
    assert!(client.tls_summary(true).is_none());
    assert!(!client.has_buffered_input());
    client.read_ready_packets().unwrap();
    assert!(!client.has_buffered_input());
    drop(client);
    broker.join().unwrap();
}

// --- self-signed broker certificates ----------------------------------------------

/// A broker whose only certificate is self-signed. `ca` selects whether that
/// certificate also claims to be a CA (OpenSSL's default for `req -x509`).
fn self_signed_identity(ca: bool) -> (ServerIdentity, String) {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    params
        .subject_alt_names
        .push(SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    if ca {
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::DigitalSignature,
        ];
    }
    let certificate = params.self_signed(&key).unwrap();
    let pem = certificate.pem();
    (
        ServerIdentity {
            chain: vec![certificate.der().clone()],
            key: PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
        },
        pem,
    )
}

#[test]
fn a_self_signed_broker_certificate_is_trusted_only_when_it_is_not_a_ca() {
    let fixture = Fixture::new("tls-self-signed");

    // The broker's own certificate, listed in the CA file, is the trust anchor.
    let (identity, pem) = self_signed_identity(false);
    let file = write_file(&fixture.root, "self.pem", pem.as_bytes(), 0o600);
    let (port, broker) = spawn_tls_broker(identity.config(), serve_a_publisher);
    run_publisher_conversation(&tls_config(
        "localhost",
        port,
        &[("mqtt_tls_ca_file", &file)],
    ));
    broker.join().unwrap().unwrap();

    // `openssl req -x509` makes a CA certificate by default. The TLS library
    // refuses a CA certificate as the server's own, even when it is trusted,
    // and the error says what to do about it.
    let (identity, pem) = self_signed_identity(true);
    let file = write_file(&fixture.root, "self-ca.pem", pem.as_bytes(), 0o600);
    let (port, broker) = spawn_tls_broker(identity.config(), serve_a_publisher);
    let error = MqttClient::connect(&tls_config(
        "localhost",
        port,
        &[("mqtt_tls_ca_file", &file)],
    ))
    .err()
    .unwrap();
    assert!(error.contains("CaUsedAsEndEntity"), "{error}");
    assert!(error.contains("CA:FALSE"), "{error}");
    assert!(broker.join().unwrap().is_err());
}
