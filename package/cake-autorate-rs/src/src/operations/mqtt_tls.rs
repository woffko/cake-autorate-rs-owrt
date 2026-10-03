//! TLS transport for the native MQTT publisher.
//!
//! The publisher keeps its own small MQTT 3.1.1 client; this module only turns
//! an established TCP connection into a rustls stream. Trust comes from either
//! the built-in Mozilla roots or from one operator-supplied PEM file that
//! replaces them, so a private CA or a self-signed broker certificate is
//! trusted on its own and nothing else is. Verification can be switched off for
//! a broker whose certificate has no CA file: the traffic is then encrypted but
//! the broker is not authenticated.
//!
//! Certificate validity is judged by the system clock. A router that has not
//! synchronised its time yet may refuse a certificate as not yet valid; the
//! publisher then exits and procd restarts it.

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, RootCertStore, SignatureScheme,
    StreamOwned,
};
use std::fs::OpenOptions;
use std::io::{self, Read};
use std::net::TcpStream;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(super) type TlsStream = StreamOwned<ClientConnection, TcpStream>;

/// The whole handshake, not each read: a broker that dribbles bytes cannot
/// keep the publisher in it.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CA_FILE_BYTES: u64 = 256 * 1024;
const MAX_CA_CERTIFICATES: usize = 64;
/// The CA file decides whom the publisher trusts with the broker password, so
/// only root may own it.
const TRUSTED_FILE_OWNER: u32 = 0;

fn trusted_owner() -> u32 {
    #[cfg(test)]
    if let Some(owner) = test_support::owner() {
        return owner;
    }
    TRUSTED_FILE_OWNER
}

/// Build the client configuration for one publisher: `verify` selects real
/// certificate and name checking, `ca_file` (empty = none) replaces the
/// built-in roots.
pub(super) fn client_config(verify: bool, ca_file: &str) -> Result<Arc<ClientConfig>, String> {
    // The provider is passed explicitly: the publisher is its own process and
    // nothing installs a process-wide default there.
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|error| format!("unable to select the MQTT TLS protocol versions: {error}"))?;
    let config = if verify {
        let mut roots = RootCertStore::empty();
        if ca_file.is_empty() {
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        } else {
            for certificate in read_ca_certificates(ca_file)? {
                roots.add(certificate).map_err(|_| {
                    "the MQTT CA file holds a certificate that cannot be used as a trust anchor"
                        .to_string()
                })?;
            }
        }
        builder.with_root_certificates(roots).with_no_client_auth()
    } else {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCertificate { provider }))
            .with_no_client_auth()
    };
    Ok(Arc::new(config))
}

/// Check the CA file the way the publisher will read it, for the status
/// command. The file is allowed to appear after the configuration is saved,
/// so the plan itself only validates the path.
pub(super) fn check_ca_file(ca_file: &str) -> Result<(), String> {
    read_ca_certificates(ca_file).map(|_| ())
}

pub(super) fn read_ca_certificates(path: &str) -> Result<Vec<CertificateDer<'static>>, String> {
    if !path.starts_with('/') || path.len() > 1024 {
        return Err("the MQTT CA file path must be absolute and at most 1024 bytes".to_string());
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| {
            if error.raw_os_error() == Some(libc::ELOOP) {
                "the MQTT CA file must not be a symbolic link".to_string()
            } else {
                format!("unable to open the MQTT CA file: {error}")
            }
        })?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("unable to inspect the MQTT CA file: {error}"))?;
    if !metadata.is_file() {
        return Err("the MQTT CA file is not a regular file".to_string());
    }
    if metadata.uid() != trusted_owner() {
        return Err("the MQTT CA file must be owned by root".to_string());
    }
    if metadata.mode() & 0o022 != 0 {
        return Err("the MQTT CA file must not be writable by group or others".to_string());
    }
    if metadata.len() == 0 || metadata.len() > MAX_CA_FILE_BYTES {
        return Err("the MQTT CA file is empty or larger than 256 KiB".to_string());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_CA_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("unable to read the MQTT CA file: {error}"))?;
    if bytes.len() as u64 > MAX_CA_FILE_BYTES {
        return Err("the MQTT CA file is larger than 256 KiB".to_string());
    }
    let mut certificates = Vec::new();
    for item in CertificateDer::pem_slice_iter(&bytes) {
        let certificate = item.map_err(|_| "the MQTT CA file is not valid PEM".to_string())?;
        if certificates.len() == MAX_CA_CERTIFICATES {
            return Err("the MQTT CA file holds more than 64 certificates".to_string());
        }
        certificates.push(certificate);
    }
    if certificates.is_empty() {
        return Err("the MQTT CA file contains no certificate".to_string());
    }
    Ok(certificates)
}

/// Run the TLS handshake on a connected socket. The socket keeps the read and
/// write timeouts the caller set; the handshake as a whole is also bounded.
pub(super) fn connect(
    config: &Arc<ClientConfig>,
    host: &str,
    mut socket: TcpStream,
) -> Result<TlsStream, String> {
    let name = ServerName::try_from(host.to_string())
        .map_err(|_| "the MQTT broker address is not a valid TLS server name".to_string())?;
    let mut connection = ClientConnection::new(config.clone(), name)
        .map_err(|error| format!("unable to start the MQTT TLS client: {error}"))?;
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    while connection.is_handshaking() {
        if Instant::now() >= deadline {
            return Err("MQTT TLS handshake timed out".to_string());
        }
        connection.complete_io(&mut socket).map_err(|error| {
            let mut message = format!("MQTT TLS handshake failed: {}", describe(&error));
            if message.contains("CaUsedAsEndEntity") {
                message.push_str(
                    ". The broker presents a CA certificate as its own server certificate; \
                     issue a separate server certificate from that CA, or create the \
                     self-signed certificate with basicConstraints CA:FALSE",
                );
            }
            message
        })?;
    }
    Ok(StreamOwned::new(connection, socket))
}

/// True when decrypted application data is already waiting inside rustls.
/// `poll` on the socket cannot see it: one TLS record may carry several MQTT
/// packets, and after the first is read the others are no longer on the wire.
pub(super) fn has_buffered_input(stream: &mut TlsStream) -> bool {
    match stream.conn.process_new_packets() {
        Ok(state) => state.plaintext_bytes_to_read() > 0,
        // A fatal alert or a protocol error: the next read reports it.
        Err(_) => true,
    }
}

/// Politely end the TLS session after the MQTT DISCONNECT was written.
pub(super) fn close(stream: &mut TlsStream) {
    stream.conn.send_close_notify();
    let _ = stream.conn.complete_io(&mut stream.sock);
}

/// One line for the system log after a successful handshake.
pub(super) fn summary(stream: &TlsStream, verified: bool) -> String {
    let version = stream
        .conn
        .protocol_version()
        .map(|version| format!("{version:?}"))
        .unwrap_or_else(|| "unknown version".to_string());
    format!(
        "MQTT TLS session established ({version}, server certificate {})",
        if verified { "verified" } else { "NOT verified" }
    )
}

fn describe(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => "timed out".to_string(),
        io::ErrorKind::UnexpectedEof
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::BrokenPipe => {
            "the broker closed the connection during the handshake (is this a TLS port?)"
                .to_string()
        }
        _ => error.to_string(),
    }
}

/// `mqtt_tls_verify=0`: accept whatever certificate the broker presents. The
/// handshake signature is still checked, so the session is encrypted to the
/// holder of the presented key, but nothing says who that is.
#[derive(Debug)]
struct AcceptAnyCertificate {
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for AcceptAnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
pub(super) mod test_support {
    use std::cell::Cell;

    thread_local! {
        static OWNER: Cell<Option<u32>> = const { Cell::new(None) };
    }

    pub(super) fn owner() -> Option<u32> {
        OWNER.with(Cell::get)
    }

    /// Unit tests are not root: let this thread trust CA files owned by
    /// `owner` until the guard is dropped.
    pub(crate) struct TrustedOwner;

    impl TrustedOwner {
        pub(crate) fn set(owner: u32) -> Self {
            OWNER.with(|value| value.set(Some(owner)));
            Self
        }
    }

    impl Drop for TrustedOwner {
        fn drop(&mut self) {
            OWNER.with(|value| value.set(None));
        }
    }
}
