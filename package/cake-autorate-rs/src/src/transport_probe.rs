use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, Once};
use std::thread;
use std::time::{Duration, Instant};
use tungstenite::client::IntoClientRequest;
use tungstenite::http::HeaderValue;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{client_tls_with_config, Message, WebSocket};

const DEFAULT_STREAMS: usize = 4;
const STREAM_DELAY: Duration = Duration::from_millis(50);
const MAX_HTTP_HEADER_BYTES: usize = 64 * 1024;
const MAX_HTTP_TOTAL_BYTES: usize = 64 * 1024;
const MAX_WS_FRAME_BYTES: usize = 16 * 1024;
const MAX_WS_BATCH_BYTES: usize = 64 * 1024;
const MAX_WS_IRRELEVANT_MESSAGES: usize = 32;

static INSTALL_CRYPTO_PROVIDER: Once = Once::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportProbeBackend {
    WebSocket,
    TcpConnect,
    PersistentHttp,
    LegacyHttp,
}

impl TransportProbeBackend {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "websocket" | "ws" => Some(Self::WebSocket),
            "tcp" | "tcp-connect" => Some(Self::TcpConnect),
            "http" | "persistent-http" => Some(Self::PersistentHttp),
            "legacy-http" => Some(Self::LegacyHttp),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::WebSocket => "websocket",
            Self::TcpConnect => "tcp-connect",
            Self::PersistentHttp => "persistent-http",
            Self::LegacyHttp => "legacy-http",
        }
    }

    pub fn trusted(self) -> bool {
        !matches!(self, Self::LegacyHttp)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RouteBinding {
    pub device: String,
    pub source_ip: String,
    pub fwmark: String,
}

impl RouteBinding {
    fn parsed_source(&self) -> Result<Option<Ipv4Addr>, String> {
        if self.source_ip.trim().is_empty() {
            return Ok(None);
        }
        self.source_ip
            .parse::<Ipv4Addr>()
            .map(Some)
            .map_err(|_| format!("invalid route source IPv4: {}", self.source_ip))
    }

    fn parsed_mark(&self) -> Result<Option<u32>, String> {
        let value = self.fwmark.trim();
        if value.is_empty() {
            return Ok(None);
        }
        let value = value.strip_prefix("0x").unwrap_or(value);
        u32::from_str_radix(value, 16)
            .map(Some)
            .map_err(|_| format!("invalid route fwmark: {}", self.fwmark))
    }
}

#[derive(Clone, Debug)]
pub struct TransportProbeSample {
    pub backend: TransportProbeBackend,
    pub endpoint: String,
    pub rtt_ms: f64,
    pub raw_samples_ms: Vec<f64>,
    pub discarded_samples: usize,
    pub server_processing_ms: f64,
    pub trusted: bool,
    pub connection_reused: bool,
}

#[derive(Clone, Debug)]
struct ParsedEndpoint {
    scheme: String,
    host: String,
    port: u16,
    path: String,
}

impl ParsedEndpoint {
    fn parse(value: &str) -> Result<Self, String> {
        if value.is_empty() || value.len() > 512 || value.chars().any(char::is_whitespace) {
            return Err(
                "transport endpoint is empty, too long, or contains whitespace".to_string(),
            );
        }
        if value.bytes().any(|byte| byte.is_ascii_control()) {
            return Err("transport endpoint contains control characters".to_string());
        }
        let (scheme, rest) = value
            .split_once("://")
            .ok_or_else(|| format!("endpoint has no URL scheme: {value}"))?;
        let scheme = scheme.to_ascii_lowercase();
        if !matches!(scheme.as_str(), "ws" | "wss" | "http" | "https" | "tcp") {
            return Err(format!("unsupported transport endpoint scheme: {scheme}"));
        }
        let (authority, path) = rest
            .split_once('/')
            .map(|(authority, path)| (authority, format!("/{path}")))
            .unwrap_or((rest, "/".to_string()));
        if authority.is_empty() || authority.contains('@') {
            return Err("transport endpoint authority is invalid".to_string());
        }
        let (host, port) = if let Some(stripped) = authority.strip_prefix('[') {
            let (host, tail) = stripped
                .split_once(']')
                .ok_or_else(|| "invalid bracketed endpoint host".to_string())?;
            let port = tail
                .strip_prefix(':')
                .map(|value| value.parse::<u16>())
                .transpose()
                .map_err(|_| "invalid endpoint port".to_string())?
                .unwrap_or_else(|| default_port(&scheme));
            (host.to_string(), port)
        } else if authority.matches(':').count() == 1 {
            let (host, port) = authority
                .rsplit_once(':')
                .ok_or_else(|| "invalid endpoint authority".to_string())?;
            let port = port
                .parse::<u16>()
                .map_err(|_| "invalid endpoint port".to_string())?;
            (host.to_string(), port)
        } else {
            (authority.to_string(), default_port(&scheme))
        };
        if host.is_empty() || port == 0 {
            return Err("transport endpoint host or port is invalid".to_string());
        }
        Ok(Self {
            scheme,
            host,
            port,
            path,
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct ProbeDeadline(Instant);

impl ProbeDeadline {
    fn after(timeout: Duration) -> Result<Self, String> {
        if timeout.is_zero() {
            return Err("transport probe timeout must be positive".to_string());
        }
        Instant::now()
            .checked_add(timeout)
            .map(Self)
            .ok_or_else(|| "transport probe deadline overflow".to_string())
    }

    fn remaining(self, operation: &str) -> Result<Duration, String> {
        self.0
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| format!("transport probe deadline exceeded during {operation}"))
    }

    fn ensure(self, operation: &str) -> Result<(), String> {
        self.remaining(operation).map(|_| ())
    }
}

#[derive(Clone, Debug)]
struct DeadlineControl(Arc<Mutex<ProbeDeadline>>);

impl DeadlineControl {
    fn new(deadline: ProbeDeadline) -> Self {
        Self(Arc::new(Mutex::new(deadline)))
    }

    fn set(&self, deadline: ProbeDeadline) {
        *self.0.lock().expect("deadline mutex poisoned") = deadline;
    }

    fn get(&self) -> std::io::Result<ProbeDeadline> {
        self.0
            .lock()
            .map(|deadline| *deadline)
            .map_err(|_| std::io::Error::other("transport deadline mutex poisoned"))
    }
}

#[derive(Debug)]
struct DeadlineTcpStream {
    inner: TcpStream,
    deadline: DeadlineControl,
}

impl DeadlineTcpStream {
    fn new(inner: TcpStream, deadline: DeadlineControl) -> Self {
        Self { inner, deadline }
    }

    fn prepare_read(&self) -> std::io::Result<ProbeDeadline> {
        let deadline = self.deadline.get()?;
        let remaining = deadline
            .0
            .checked_duration_since(Instant::now())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::TimedOut, "probe deadline"))?;
        self.inner
            .set_read_timeout(Some(remaining.max(Duration::from_millis(1))))?;
        Ok(deadline)
    }

    fn prepare_write(&self) -> std::io::Result<ProbeDeadline> {
        let deadline = self.deadline.get()?;
        let remaining = deadline
            .0
            .checked_duration_since(Instant::now())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::TimedOut, "probe deadline"))?;
        self.inner
            .set_write_timeout(Some(remaining.max(Duration::from_millis(1))))?;
        Ok(deadline)
    }

    fn reject_late<T>(deadline: ProbeDeadline, result: std::io::Result<T>) -> std::io::Result<T> {
        if Instant::now() >= deadline.0 {
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "transport probe absolute deadline exceeded",
            ))
        } else {
            result
        }
    }
}

impl Read for DeadlineTcpStream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let deadline = self.prepare_read()?;
        let result = self.inner.read(buffer);
        Self::reject_late(deadline, result)
    }
}

impl Write for DeadlineTcpStream {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let deadline = self.prepare_write()?;
        let result = self.inner.write(buffer);
        Self::reject_late(deadline, result)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let deadline = self.prepare_write()?;
        let result = self.inner.flush();
        Self::reject_late(deadline, result)
    }
}

impl tungstenite::stream::NoDelay for DeadlineTcpStream {
    fn set_nodelay(&mut self, nodelay: bool) -> std::io::Result<()> {
        self.inner.set_nodelay(nodelay)
    }
}

type ResolveResult = Result<Vec<SocketAddr>, String>;

struct Resolver {
    host: String,
    port: u16,
    completed: Option<ResolveResult>,
    pending: Option<PendingResolution>,
}

struct PendingResolution {
    receiver: Receiver<ResolveResult>,
    handle: thread::JoinHandle<()>,
}

impl Resolver {
    fn new(parsed: &ParsedEndpoint) -> Self {
        let completed = parsed.host.parse::<IpAddr>().ok().map(|ip| {
            if ip.is_ipv4() {
                Ok(vec![SocketAddr::new(ip, parsed.port)])
            } else {
                Err(format!("{} has no IPv4 address", parsed.host))
            }
        });
        Self {
            host: parsed.host.clone(),
            port: parsed.port,
            completed,
            pending: None,
        }
    }

    #[cfg(test)]
    fn from_addresses(host: &str, addresses: Vec<SocketAddr>) -> Self {
        Self {
            host: host.to_string(),
            port: addresses.first().map(SocketAddr::port).unwrap_or(443),
            completed: Some(Ok(addresses)),
            pending: None,
        }
    }

    fn resolve(&mut self, deadline: ProbeDeadline) -> ResolveResult {
        if let Some(result) = &self.completed {
            deadline.ensure("DNS cache lookup")?;
            return result.clone();
        }
        if self.pending.is_none() {
            let host = self.host.clone();
            let port = self.port;
            let (sender, receiver) = mpsc::sync_channel(1);
            let handle = thread::Builder::new()
                .name("cake-autorate-dns".to_string())
                .spawn(move || {
                    let result = resolve_ipv4(&host, port);
                    let _ = sender.send(result);
                })
                .map_err(|error| format!("failed to start DNS resolver: {error}"))?;
            self.pending = Some(PendingResolution { receiver, handle });
        }
        let remaining = deadline.remaining("DNS resolution")?;
        let result = match self
            .pending
            .as_ref()
            .expect("resolver receiver exists")
            .receiver
            .recv_timeout(remaining)
        {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => {
                return Err("transport probe deadline exceeded during DNS resolution".to_string())
            }
            Err(RecvTimeoutError::Disconnected) => {
                Err("DNS resolver terminated without a result".to_string())
            }
        };
        if let Some(pending) = self.pending.take() {
            let _ = pending.handle.join();
        }
        self.completed = Some(result.clone());
        deadline.ensure("DNS resolution")?;
        result
    }
}

fn resolve_ipv4(host: &str, port: u16) -> ResolveResult {
    let mut addresses = (host, port)
        .to_socket_addrs()
        .map_err(|error| format!("failed to resolve {host}: {error}"))?
        .filter(SocketAddr::is_ipv4)
        .collect::<Vec<_>>();
    addresses.sort();
    addresses.dedup();
    if addresses.is_empty() {
        return Err(format!("{host} has no IPv4 address"));
    }
    Ok(addresses)
}

fn default_port(scheme: &str) -> u16 {
    match scheme {
        "wss" | "https" => 443,
        "ws" | "http" => 80,
        _ => 443,
    }
}

pub struct TransportProbeEngine {
    backend: TransportProbeBackend,
    endpoint: String,
    binding: RouteBinding,
    timeout: Duration,
    resolver: Resolver,
    websocket: Option<WebSocketProbe>,
    http: Option<HttpProbe>,
}

impl TransportProbeEngine {
    pub fn new(
        backend: TransportProbeBackend,
        endpoint: String,
        binding: RouteBinding,
        timeout: Duration,
    ) -> Result<Self, String> {
        let parsed = ParsedEndpoint::parse(&endpoint)?;
        match backend {
            TransportProbeBackend::WebSocket if !matches!(parsed.scheme.as_str(), "ws" | "wss") => {
                return Err("websocket backend requires ws:// or wss:// endpoint".to_string())
            }
            TransportProbeBackend::TcpConnect => {}
            TransportProbeBackend::PersistentHttp if parsed.scheme != "https" => {
                return Err("persistent HTTP backend requires https:// endpoint".to_string())
            }
            TransportProbeBackend::LegacyHttp => {}
            _ => {}
        }
        let resolver = Resolver::new(&parsed);
        let websocket = (backend == TransportProbeBackend::WebSocket).then(|| {
            WebSocketProbe::new(endpoint.clone(), parsed.clone(), binding.clone(), timeout)
        });
        let http = (backend == TransportProbeBackend::PersistentHttp)
            .then(|| HttpProbe::new(endpoint.clone(), parsed, binding.clone(), timeout));
        Ok(Self {
            backend,
            endpoint,
            binding,
            timeout,
            resolver,
            websocket,
            http,
        })
    }

    pub fn probe(&mut self) -> Result<TransportProbeSample, String> {
        let deadline = ProbeDeadline::after(self.timeout)?;
        if self.backend == TransportProbeBackend::LegacyHttp {
            return probe_legacy_http(&self.endpoint, deadline);
        }
        let addresses = self.resolver.resolve(deadline)?;
        match self.backend {
            TransportProbeBackend::WebSocket => self
                .websocket
                .as_mut()
                .ok_or_else(|| "websocket probe is unavailable".to_string())?
                .probe(&addresses, deadline),
            TransportProbeBackend::TcpConnect => probe_tcp_batch(
                &self.endpoint,
                &addresses,
                &self.binding,
                deadline,
                DEFAULT_STREAMS,
            ),
            TransportProbeBackend::PersistentHttp => self
                .http
                .as_mut()
                .ok_or_else(|| "persistent HTTP probe is unavailable".to_string())?
                .probe(&addresses, deadline),
            TransportProbeBackend::LegacyHttp => unreachable!("handled before DNS"),
        }
    }
}

type WsStream = WebSocket<MaybeTlsStream<DeadlineTcpStream>>;

struct WebSocketProbe {
    endpoint: String,
    parsed: ParsedEndpoint,
    binding: RouteBinding,
    stream: Option<WsStream>,
    deadline: DeadlineControl,
    sequence: u64,
}

impl WebSocketProbe {
    fn new(
        endpoint: String,
        parsed: ParsedEndpoint,
        binding: RouteBinding,
        timeout: Duration,
    ) -> Self {
        let initial_deadline = ProbeDeadline::after(timeout)
            .expect("validated transport timeout always forms a deadline");
        Self {
            endpoint,
            parsed,
            binding,
            stream: None,
            deadline: DeadlineControl::new(initial_deadline),
            sequence: 1,
        }
    }

    fn connect(
        &mut self,
        addresses: &[SocketAddr],
        deadline: ProbeDeadline,
    ) -> Result<WsStream, String> {
        install_crypto_provider();
        self.deadline.set(deadline);
        let (tcp, _) = connect_route_aware(
            addresses,
            &self.binding,
            deadline,
            self.deadline.clone(),
            false,
        )?;
        let mut request = self
            .endpoint
            .as_str()
            .into_client_request()
            .map_err(|error| format!("invalid WebSocket request: {error}"))?;
        let origin_scheme = if self.parsed.scheme == "wss" {
            "https"
        } else {
            "http"
        };
        let default_origin_port = default_port(&self.parsed.scheme);
        let origin = if self.parsed.port == default_origin_port {
            format!("{origin_scheme}://{}", self.parsed.host)
        } else {
            format!(
                "{origin_scheme}://{}:{}",
                self.parsed.host, self.parsed.port
            )
        };
        request.headers_mut().insert(
            "Origin",
            HeaderValue::from_str(&origin)
                .map_err(|error| format!("invalid WebSocket Origin header: {error}"))?,
        );
        let config = WebSocketConfig::default()
            .read_buffer_size(4 * 1024)
            .write_buffer_size(0)
            .max_write_buffer_size(MAX_WS_FRAME_BYTES * 2)
            .max_message_size(Some(MAX_WS_FRAME_BYTES))
            .max_frame_size(Some(MAX_WS_FRAME_BYTES));
        let (mut stream, _) = client_tls_with_config(request, tcp, Some(config), None)
            .map_err(|error| format!("WebSocket handshake failed: {error}"))?;
        deadline.ensure("WebSocket handshake")?;
        Self::raw_batch_on(&mut stream, &mut self.sequence, 1, deadline)?;
        deadline.ensure("WebSocket warmup")?;
        Ok(stream)
    }

    fn probe(
        &mut self,
        addresses: &[SocketAddr],
        deadline: ProbeDeadline,
    ) -> Result<TransportProbeSample, String> {
        let connection_reused = self.stream.is_some();
        self.deadline.set(deadline);
        let result = (|| {
            if self.stream.is_none() {
                let stream = self.connect(addresses, deadline)?;
                // A connection becomes reusable only after its warmup succeeds.
                self.stream = Some(stream);
            }
            let (raw, server_processing) = Self::raw_batch_on(
                self.stream
                    .as_mut()
                    .ok_or_else(|| "WebSocket is disconnected".to_string())?,
                &mut self.sequence,
                DEFAULT_STREAMS,
                deadline,
            )?;
            let (rtt_ms, evidence, discarded_samples) = summarize_probe_samples(&raw)
                .ok_or_else(|| "WebSocket returned no valid RTT samples".to_string())?;
            deadline.ensure("WebSocket batch")?;
            Ok(TransportProbeSample {
                backend: TransportProbeBackend::WebSocket,
                endpoint: self.endpoint.clone(),
                rtt_ms,
                discarded_samples,
                raw_samples_ms: evidence,
                server_processing_ms: server_processing,
                trusted: true,
                connection_reused,
            })
        })();
        if result.is_err() {
            self.stream = None;
        }
        result
    }

    fn raw_batch_on(
        ws: &mut WsStream,
        sequence: &mut u64,
        streams: usize,
        deadline: ProbeDeadline,
    ) -> Result<(Vec<f64>, f64), String> {
        let mut samples = Vec::with_capacity(streams.max(1));
        let mut processing_total_ms = 0.0;
        let mut irrelevant_messages = 0usize;
        let mut irrelevant_bytes = 0usize;
        for stream_id in 1..=streams.max(1) {
            deadline.ensure("WebSocket batch")?;
            let token = *sequence;
            *sequence = sequence.wrapping_add(1).max(1);
            let payload = format!(
                "{{\"type\":\"ping\",\"timestamp\":{token},\"streamId\":{stream_id},\"payloadSizeBytes\":0,\"payload\":\"\"}}"
            );
            let started = Instant::now();
            ws.send(Message::Text(payload.into()))
                .map_err(|error| format!("WebSocket ping send failed: {error}"))?;
            loop {
                deadline.ensure("WebSocket pong")?;
                let message = ws
                    .read()
                    .map_err(|error| format!("WebSocket pong read failed: {error}"))?;
                match message {
                    Message::Text(text) => {
                        let Some(received_token) = json_u64(&text, "clientTime") else {
                            count_irrelevant_message(
                                &mut irrelevant_messages,
                                &mut irrelevant_bytes,
                                text.len(),
                            )?;
                            continue;
                        };
                        if received_token != token {
                            count_irrelevant_message(
                                &mut irrelevant_messages,
                                &mut irrelevant_bytes,
                                text.len(),
                            )?;
                            continue;
                        }
                        let processing_ms =
                            json_number_strict(&text, "serverProcessingTime")?.unwrap_or(0.0);
                        if !processing_ms.is_finite() || !(0.0..=60_000.0).contains(&processing_ms)
                        {
                            return Err(
                                "WebSocket returned invalid server processing time".to_string()
                            );
                        }
                        let rtt_ms = started.elapsed().as_secs_f64() * 1000.0 - processing_ms;
                        if rtt_ms.is_finite() && rtt_ms > 0.0 {
                            samples.push(rtt_ms);
                            processing_total_ms += processing_ms;
                        }
                        break;
                    }
                    Message::Ping(payload) => {
                        count_irrelevant_message(
                            &mut irrelevant_messages,
                            &mut irrelevant_bytes,
                            payload.len(),
                        )?;
                        ws.send(Message::Pong(payload))
                            .map_err(|error| format!("WebSocket pong send failed: {error}"))?;
                    }
                    Message::Close(_) => return Err("WebSocket closed during probe".to_string()),
                    other => {
                        count_irrelevant_message(
                            &mut irrelevant_messages,
                            &mut irrelevant_bytes,
                            message_payload_len(&other),
                        )?;
                    }
                }
            }
            if stream_id < streams {
                let remaining = deadline.remaining("WebSocket inter-sample delay")?;
                thread::sleep(STREAM_DELAY.min(remaining));
            }
        }
        if samples.is_empty() {
            return Err("WebSocket batch contained no valid pong".to_string());
        }
        Ok((samples, processing_total_ms / streams.max(1) as f64))
    }
}

struct HttpProbe {
    endpoint: String,
    parsed: ParsedEndpoint,
    binding: RouteBinding,
    tls_config: Arc<ClientConfig>,
    stream: Option<StreamOwned<ClientConnection, DeadlineTcpStream>>,
    deadline: DeadlineControl,
    sequence: u64,
}

impl HttpProbe {
    fn new(
        endpoint: String,
        parsed: ParsedEndpoint,
        binding: RouteBinding,
        timeout: Duration,
    ) -> Self {
        install_crypto_provider();
        let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let mut config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let initial_deadline = ProbeDeadline::after(timeout)
            .expect("validated transport timeout always forms a deadline");
        Self {
            endpoint,
            parsed,
            binding,
            tls_config: Arc::new(config),
            stream: None,
            deadline: DeadlineControl::new(initial_deadline),
            sequence: 1,
        }
    }

    fn connect(
        &mut self,
        addresses: &[SocketAddr],
        deadline: ProbeDeadline,
    ) -> Result<StreamOwned<ClientConnection, DeadlineTcpStream>, String> {
        if self.parsed.scheme != "https" {
            return Err("persistent HTTP currently requires https://".to_string());
        }
        self.deadline.set(deadline);
        let (tcp, _) = connect_route_aware(
            addresses,
            &self.binding,
            deadline,
            self.deadline.clone(),
            false,
        )?;
        let server_name = ServerName::try_from(self.parsed.host.clone())
            .map_err(|_| "invalid HTTP TLS server name".to_string())?;
        let connection = ClientConnection::new(self.tls_config.clone(), server_name)
            .map_err(|error| format!("failed to create HTTP TLS client: {error}"))?;
        let mut stream = StreamOwned::new(connection, tcp);
        while stream.conn.is_handshaking() {
            stream
                .conn
                .complete_io(&mut stream.sock)
                .map_err(|error| format!("HTTP TLS handshake failed: {error}"))?;
        }
        deadline.ensure("HTTP TLS handshake")?;
        Self::raw_probe_on(&mut stream, &self.parsed, &mut self.sequence, deadline)?;
        deadline.ensure("HTTP warmup")?;
        Ok(stream)
    }

    fn probe(
        &mut self,
        addresses: &[SocketAddr],
        deadline: ProbeDeadline,
    ) -> Result<TransportProbeSample, String> {
        let connection_reused = self.stream.is_some();
        self.deadline.set(deadline);
        let result = (|| {
            if self.stream.is_none() {
                let stream = self.connect(addresses, deadline)?;
                // Never publish a half-initialized connection: warmup completed.
                self.stream = Some(stream);
            }
            let mut raw = Vec::with_capacity(DEFAULT_STREAMS);
            let mut processing_total = 0.0;
            for _ in 0..DEFAULT_STREAMS {
                deadline.ensure("persistent HTTP batch")?;
                let (rtt_ms, processing_ms) = Self::raw_probe_on(
                    self.stream
                        .as_mut()
                        .ok_or_else(|| "persistent HTTP is disconnected".to_string())?,
                    &self.parsed,
                    &mut self.sequence,
                    deadline,
                )?;
                raw.push(rtt_ms);
                processing_total += processing_ms;
            }
            let (rtt_ms, evidence, discarded_samples) = summarize_probe_samples(&raw)
                .ok_or_else(|| "persistent HTTP returned no valid RTT samples".to_string())?;
            deadline.ensure("persistent HTTP batch")?;
            Ok(TransportProbeSample {
                backend: TransportProbeBackend::PersistentHttp,
                endpoint: self.endpoint.clone(),
                rtt_ms,
                discarded_samples,
                raw_samples_ms: evidence,
                server_processing_ms: processing_total / DEFAULT_STREAMS as f64,
                trusted: true,
                connection_reused,
            })
        })();
        if result.is_err() {
            self.stream = None;
        }
        result
    }

    fn raw_probe_on(
        stream: &mut StreamOwned<ClientConnection, DeadlineTcpStream>,
        parsed: &ParsedEndpoint,
        sequence: &mut u64,
        deadline: ProbeDeadline,
    ) -> Result<(f64, f64), String> {
        let separator = if parsed.path.contains('?') { '&' } else { '?' };
        let request = format!(
            "GET {}{}t={} HTTP/1.1\r\nHost: {}\r\nUser-Agent: cake-autorate-rs/rc17\r\nAccept: application/json\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n",
            parsed.path, separator, *sequence, parsed.host
        );
        *sequence = sequence.wrapping_add(1).max(1);
        deadline.ensure("persistent HTTP request")?;
        let started = Instant::now();
        stream
            .write_all(request.as_bytes())
            .and_then(|_| stream.flush())
            .map_err(|error| format!("persistent HTTP request failed: {error}"))?;
        let response = read_http_response(stream)?;
        deadline.ensure("persistent HTTP response")?;
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        let processing_ms = match server_processing_ms(&response.headers)? {
            Some(value) if value.is_finite() && (0.0..=60_000.0).contains(&value) => value,
            Some(_) => {
                return Err("persistent HTTP returned invalid server processing time".to_string())
            }
            None => 0.0,
        };
        let rtt_ms = elapsed_ms - processing_ms;
        if !rtt_ms.is_finite() || rtt_ms <= 0.0 {
            return Err("persistent HTTP returned invalid network RTT".to_string());
        }
        Ok((rtt_ms, processing_ms))
    }
}

fn install_crypto_provider() {
    INSTALL_CRYPTO_PROVIDER.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn connect_route_aware(
    addresses: &[SocketAddr],
    binding: &RouteBinding,
    deadline: ProbeDeadline,
    deadline_control: DeadlineControl,
    timed: bool,
) -> Result<(DeadlineTcpStream, f64), String> {
    let source = binding.parsed_source()?;
    let mark = binding.parsed_mark()?;
    let mut errors = Vec::new();
    let addresses = addresses
        .iter()
        .copied()
        .filter(SocketAddr::is_ipv4)
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err("route-aware TCP connection has no IPv4 address".to_string());
    }
    for (index, address) in addresses.iter().copied().enumerate() {
        let remaining = deadline.remaining("TCP connect")?;
        let attempts_left = (addresses.len() - index) as u32;
        let attempt_budget = (remaining / attempts_left).max(Duration::from_millis(1));
        let socket = match Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)) {
            Ok(socket) => socket,
            Err(error) => {
                errors.push(error.to_string());
                continue;
            }
        };
        if !binding.device.trim().is_empty() {
            if let Err(error) = socket.bind_device(Some(binding.device.as_bytes())) {
                errors.push(format!("bind {}: {error}", binding.device));
                continue;
            }
        }
        if let Some(mark) = mark {
            if let Err(error) = socket.set_mark(mark) {
                errors.push(format!("SO_MARK {mark:#x}: {error}"));
                continue;
            }
        }
        if let Some(source) = source {
            if let Err(error) = socket.bind(&SockAddr::from(SocketAddr::new(IpAddr::V4(source), 0)))
            {
                errors.push(format!("bind source {source}: {error}"));
                continue;
            }
        }
        let started = Instant::now();
        if let Err(error) = socket.connect_timeout(&SockAddr::from(address), attempt_budget) {
            errors.push(format!("connect {address}: {error}"));
            continue;
        }
        deadline.ensure("TCP connect")?;
        let connect_ms = if timed {
            started.elapsed().as_secs_f64() * 1000.0
        } else {
            0.0
        };
        let stream: TcpStream = socket.into();
        stream
            .set_nodelay(true)
            .map_err(|error| format!("failed to set TCP_NODELAY: {error}"))?;
        return Ok((DeadlineTcpStream::new(stream, deadline_control), connect_ms));
    }
    deadline.ensure("TCP connect")?;
    Err(format!(
        "route-aware TCP connection failed: {}",
        errors.join("; ")
    ))
}

fn probe_tcp_batch(
    endpoint: &str,
    addresses: &[SocketAddr],
    binding: &RouteBinding,
    deadline: ProbeDeadline,
    streams: usize,
) -> Result<TransportProbeSample, String> {
    let mut raw = Vec::with_capacity(streams);
    for index in 0..streams.max(1) {
        let control = DeadlineControl::new(deadline);
        let (_, connect_ms) = connect_route_aware(addresses, binding, deadline, control, true)?;
        if connect_ms.is_finite() && connect_ms > 0.0 {
            raw.push(connect_ms);
        }
        if index + 1 < streams.max(1) {
            let remaining = deadline.remaining("TCP inter-sample delay")?;
            thread::sleep(STREAM_DELAY.min(remaining));
        }
    }
    deadline.ensure("TCP connect batch")?;
    let (rtt_ms, evidence, discarded_samples) = summarize_probe_samples(&raw)
        .ok_or_else(|| "TCP connect returned no valid RTT samples".to_string())?;
    Ok(TransportProbeSample {
        backend: TransportProbeBackend::TcpConnect,
        endpoint: endpoint.to_string(),
        rtt_ms,
        discarded_samples,
        raw_samples_ms: evidence,
        server_processing_ms: 0.0,
        trusted: true,
        connection_reused: false,
    })
}

fn probe_legacy_http(
    endpoint: &str,
    deadline: ProbeDeadline,
) -> Result<TransportProbeSample, String> {
    let started = Instant::now();
    let timeout = deadline.remaining("legacy HTTP startup")?;
    let mut child = Command::new("uclient-fetch")
        .arg("-4")
        .arg("-q")
        .arg("-T")
        .arg(timeout.as_secs().max(1).to_string())
        .arg("-O")
        .arg("/dev/null")
        .arg(endpoint)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("failed to execute legacy uclient-fetch: {error}"))?;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("failed to wait for legacy uclient-fetch: {error}"))?
        {
            break status;
        }
        if deadline.remaining("legacy HTTP request").is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("transport probe deadline exceeded during legacy HTTP request".to_string());
        }
        thread::sleep(Duration::from_millis(5));
    };
    if !status.success() {
        return Err(format!("legacy uclient-fetch exited with {status}"));
    }
    deadline.ensure("legacy HTTP request")?;
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    Ok(TransportProbeSample {
        backend: TransportProbeBackend::LegacyHttp,
        endpoint: endpoint.to_string(),
        rtt_ms: elapsed_ms,
        raw_samples_ms: vec![elapsed_ms],
        discarded_samples: 0,
        server_processing_ms: 0.0,
        trusted: false,
        connection_reused: false,
    })
}

#[derive(Debug, PartialEq, Eq)]
struct HttpResponse {
    headers: String,
    body: Vec<u8>,
}

fn read_http_response<R: Read>(stream: &mut R) -> Result<HttpResponse, String> {
    let mut data = Vec::new();
    let mut buffer = [0u8; 4096];
    let mut header_end = None;
    let mut content_length = None;
    loop {
        let count = stream
            .read(&mut buffer)
            .map_err(|error| format!("persistent HTTP response failed: {error}"))?;
        if count == 0 {
            return Err("persistent HTTP connection closed".to_string());
        }
        data.extend_from_slice(&buffer[..count]);
        if data.len() > MAX_HTTP_TOTAL_BYTES {
            return Err("persistent HTTP response exceeds 64 KiB".to_string());
        }
        if data.len() > MAX_HTTP_HEADER_BYTES && header_end.is_none() {
            return Err("persistent HTTP response headers are too large".to_string());
        }
        if header_end.is_none() {
            header_end = find_bytes(&data, b"\r\n\r\n").map(|index| index + 4);
            if let Some(end) = header_end {
                if end > MAX_HTTP_HEADER_BYTES {
                    return Err("persistent HTTP response headers are too large".to_string());
                }
                let headers = std::str::from_utf8(&data[..end])
                    .map_err(|_| "persistent HTTP headers are not valid UTF-8".to_string())?;
                let length = parse_http_framing(headers)?;
                if end
                    .checked_add(length)
                    .filter(|total| *total <= MAX_HTTP_TOTAL_BYTES)
                    .is_none()
                {
                    return Err("persistent HTTP response exceeds 64 KiB".to_string());
                }
                content_length = Some(length);
            }
        }
        if let Some(end) = header_end {
            let needed = content_length.expect("framing parsed with header end");
            let expected = end
                .checked_add(needed)
                .ok_or_else(|| "persistent HTTP response length overflow".to_string())?;
            if data.len() > expected {
                return Err(
                    "persistent HTTP response contains ambiguous trailing bytes".to_string()
                );
            }
            if data.len() == expected {
                let headers = String::from_utf8(data[..end].to_vec())
                    .map_err(|_| "persistent HTTP headers are not valid UTF-8".to_string())?;
                return Ok(HttpResponse {
                    headers,
                    body: data[end..].to_vec(),
                });
            }
        }
    }
}

fn parse_http_framing(headers: &str) -> Result<usize, String> {
    let mut lines = headers.split("\r\n");
    let status = lines
        .next()
        .ok_or_else(|| "persistent HTTP response has no status line".to_string())?;
    let mut status_parts = status.split_ascii_whitespace();
    let version = status_parts.next().unwrap_or_default();
    let code = status_parts.next().unwrap_or_default();
    if version != "HTTP/1.1" || code != "200" {
        return Err(format!("persistent HTTP returned {status}"));
    }
    let mut content_lengths = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if line.starts_with([' ', '\t']) {
            return Err("persistent HTTP folded headers are unsupported".to_string());
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| "persistent HTTP response contains a malformed header".to_string())?;
        let name = name.trim();
        let value = value.trim();
        if name.is_empty()
            || !name.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(
                        byte,
                        b'!' | b'#'
                            | b'$'
                            | b'%'
                            | b'&'
                            | b'\''
                            | b'*'
                            | b'+'
                            | b'-'
                            | b'.'
                            | b'^'
                            | b'_'
                            | b'`'
                            | b'|'
                            | b'~'
                    )
            })
        {
            return Err("persistent HTTP response contains an invalid header name".to_string());
        }
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err("persistent HTTP Transfer-Encoding is unsupported".to_string());
        }
        if name.eq_ignore_ascii_case("content-length") {
            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err("persistent HTTP Content-Length is invalid".to_string());
            }
            content_lengths.push(
                value
                    .parse::<usize>()
                    .map_err(|_| "persistent HTTP Content-Length is invalid".to_string())?,
            );
        }
        if name.eq_ignore_ascii_case("connection")
            && value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("close"))
        {
            return Err("persistent HTTP server requested connection close".to_string());
        }
    }
    match content_lengths.as_slice() {
        [length] => Ok(*length),
        [] => Err("persistent HTTP response is missing Content-Length".to_string()),
        _ => Err("persistent HTTP response has multiple Content-Length headers".to_string()),
    }
}

fn header_value<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name).then_some(value.trim())
    })
}

fn server_processing_ms(headers: &str) -> Result<Option<f64>, String> {
    let Some(value) = header_value(headers, "server-timing") else {
        return Ok(None);
    };
    let marker = "dur=";
    let marker_index = value
        .find(marker)
        .ok_or_else(|| "persistent HTTP Server-Timing has no duration".to_string())?;
    let tail = value
        .get(marker_index + marker.len()..)
        .ok_or_else(|| "persistent HTTP Server-Timing is malformed".to_string())?;
    let end = tail
        .find(|ch: char| !(ch.is_ascii_digit() || matches!(ch, '.' | '-' | '+')))
        .unwrap_or(tail.len());
    let token = tail
        .get(..end)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "persistent HTTP Server-Timing duration is empty".to_string())?;
    let value = token
        .parse::<f64>()
        .map_err(|_| "persistent HTTP Server-Timing duration is invalid".to_string())?;
    if !value.is_finite() {
        return Err("persistent HTTP Server-Timing duration must be finite".to_string());
    }
    Ok(Some(value))
}

#[cfg(test)]
fn json_number(data: &str, key: &str) -> Option<f64> {
    let marker = format!("\"{key}\"");
    let tail = data.get(data.find(&marker)? + marker.len()..)?.trim_start();
    let tail = tail.strip_prefix(':')?.trim_start();
    if tail.starts_with("null") {
        return None;
    }
    let end = tail
        .find(|ch: char| !(ch.is_ascii_digit() || matches!(ch, '.' | '-' | '+' | 'e' | 'E')))
        .unwrap_or(tail.len());
    tail.get(..end)?.parse::<f64>().ok()
}

fn json_number_strict(data: &str, key: &str) -> Result<Option<f64>, String> {
    let marker = format!("\"{key}\"");
    let Some(marker_index) = data.find(&marker) else {
        return Ok(None);
    };
    let tail = data
        .get(marker_index + marker.len()..)
        .ok_or_else(|| format!("WebSocket {key} field is malformed"))?
        .trim_start();
    let tail = tail
        .strip_prefix(':')
        .ok_or_else(|| format!("WebSocket {key} field has no colon"))?
        .trim_start();
    if tail.starts_with("null") {
        let remainder = tail.get(4..).unwrap_or_default();
        if remainder
            .chars()
            .next()
            .map(|ch| ch.is_ascii_whitespace() || matches!(ch, ',' | '}'))
            .unwrap_or(true)
        {
            return Ok(None);
        }
        return Err(format!("WebSocket {key} null field is malformed"));
    }
    let end = tail
        .find(|ch: char| !(ch.is_ascii_digit() || matches!(ch, '.' | '-' | '+' | 'e' | 'E')))
        .unwrap_or(tail.len());
    let token = tail
        .get(..end)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| format!("WebSocket {key} field is not numeric"))?;
    let remainder = tail.get(end..).unwrap_or_default();
    if !remainder
        .chars()
        .next()
        .map(|ch| ch.is_ascii_whitespace() || matches!(ch, ',' | '}'))
        .unwrap_or(true)
    {
        return Err(format!("WebSocket {key} field has trailing data"));
    }
    let value = token
        .parse::<f64>()
        .map_err(|_| format!("WebSocket {key} field is invalid"))?;
    if !value.is_finite() {
        return Err(format!("WebSocket {key} field must be finite"));
    }
    Ok(Some(value))
}

fn json_u64(data: &str, key: &str) -> Option<u64> {
    let marker = format!("\"{key}\"");
    let tail = data.get(data.find(&marker)? + marker.len()..)?.trim_start();
    let tail = tail.strip_prefix(':')?.trim_start();
    let end = tail
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(tail.len());
    let token = tail.get(..end)?;
    if token.is_empty() {
        return None;
    }
    let remainder = tail.get(end..)?;
    if !remainder
        .chars()
        .next()
        .map(|ch| ch.is_ascii_whitespace() || matches!(ch, ',' | '}'))
        .unwrap_or(true)
    {
        return None;
    }
    token.parse::<u64>().ok()
}

fn message_payload_len(message: &Message) -> usize {
    match message {
        Message::Text(value) => value.len(),
        Message::Binary(value) | Message::Ping(value) | Message::Pong(value) => value.len(),
        Message::Close(value) => value
            .as_ref()
            .map(|frame| frame.reason.len().saturating_add(2))
            .unwrap_or(0),
        _ => 0,
    }
}

fn count_irrelevant_message(
    messages: &mut usize,
    bytes: &mut usize,
    payload_bytes: usize,
) -> Result<(), String> {
    *messages = messages.saturating_add(1);
    *bytes = bytes.saturating_add(payload_bytes);
    if *messages > MAX_WS_IRRELEVANT_MESSAGES || *bytes > MAX_WS_BATCH_BYTES {
        return Err("WebSocket irrelevant-message budget exceeded".to_string());
    }
    Ok(())
}

fn median_sorted(values: &[f64]) -> f64 {
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}

fn robust_median(values: &[f64]) -> Option<(f64, Vec<f64>)> {
    let mut sorted = values
        .iter()
        .copied()
        .filter(|value| value.is_finite() && *value > 0.0 && *value < 10_000.0)
        .collect::<Vec<_>>();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_by(f64::total_cmp);
    let center = median_sorted(&sorted);
    let filtered = if sorted.len() >= 4 {
        let mut deviations = sorted
            .iter()
            .map(|value| (value - center).abs())
            .collect::<Vec<_>>();
        deviations.sort_by(f64::total_cmp);
        let mad = median_sorted(&deviations);
        let limit = (3.5 * mad).max((center * 0.25).max(0.25));
        values
            .iter()
            .copied()
            .filter(|value| {
                value.is_finite()
                    && *value > 0.0
                    && *value < 10_000.0
                    && (*value - center).abs() <= limit
            })
            .collect::<Vec<_>>()
    } else {
        sorted
    };
    if filtered.is_empty() {
        return None;
    }
    let mut accepted = filtered;
    accepted.sort_by(f64::total_cmp);
    let median = median_sorted(&accepted);
    Some((median, accepted))
}

/// Keep robust filtering local to the single diagnostic batch median.  The
/// complete set of finite, positive, deadline-bounded RTTs is returned as
/// measurement evidence so a caller's p95 cannot silently lose the high tail
/// that Full Auto-Tune is specifically trying to detect.
fn summarize_probe_samples(values: &[f64]) -> Option<(f64, Vec<f64>, usize)> {
    let evidence = values
        .iter()
        .copied()
        .filter(|value| value.is_finite() && *value > 0.0 && *value < 10_000.0)
        .collect::<Vec<_>>();
    let (median, accepted) = robust_median(&evidence)?;
    let discarded = values.len().saturating_sub(accepted.len());
    Some((median, evidence, discarded))
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::net::TcpListener;

    #[test]
    fn parses_transport_endpoints_and_route_binding() {
        let endpoint = ParsedEndpoint::parse("wss://example.com/ws?q=1").unwrap();
        assert_eq!(endpoint.host, "example.com");
        assert_eq!(endpoint.port, 443);
        assert_eq!(endpoint.path, "/ws?q=1");
        let binding = RouteBinding {
            device: "pppoe-wan".to_string(),
            source_ip: "192.0.2.2".to_string(),
            fwmark: "0x200".to_string(),
        };
        assert_eq!(
            binding.parsed_source().unwrap(),
            Some(Ipv4Addr::new(192, 0, 2, 2))
        );
        assert_eq!(binding.parsed_mark().unwrap(), Some(0x200));
    }

    #[test]
    fn robust_median_matches_four_stream_filtering() {
        let (median, kept) = robust_median(&[10.0, 11.0, 12.0, 5000.0]).unwrap();
        assert_eq!(median, 11.0);
        assert_eq!(kept, vec![10.0, 11.0, 12.0]);
        let (median, kept) = robust_median(&[10.0, 10.5, 11.0, 11.5, 5000.0]).unwrap();
        assert_eq!(median, 10.75);
        assert_eq!(kept, vec![10.0, 10.5, 11.0, 11.5]);
    }

    #[test]
    fn high_tail_is_preserved_for_full_autotune_p95_evidence() {
        let (median, evidence, discarded) =
            summarize_probe_samples(&[10.0, 10.0, 10.0, 200.0]).unwrap();

        assert_eq!(median, 10.0);
        assert_eq!(evidence, vec![10.0, 10.0, 10.0, 200.0]);
        assert_eq!(discarded, 1);
    }

    #[test]
    fn parses_server_timing_and_json_numbers() {
        assert_eq!(
            server_processing_ms("HTTP/1.1 200 OK\r\nServer-Timing: processing;dur=1.25\r\n"),
            Ok(Some(1.25))
        );
        assert_eq!(
            json_number("{\"clientTime\":42,\"x\":1}", "clientTime"),
            Some(42.0)
        );
        assert_eq!(json_number("{\"clientTime\":null}", "clientTime"), None);
        assert_eq!(json_u64("{\"clientTime\":42}", "clientTime"), Some(42));
        assert_eq!(json_u64("{\"clientTime\":42.5}", "clientTime"), None);
        assert!(
            json_number_strict("{\"serverProcessingTime\":NaN}", "serverProcessingTime").is_err()
        );
        assert!(
            server_processing_ms("HTTP/1.1 200 OK\r\nServer-Timing: processing;dur=NaN\r\n")
                .is_err()
        );
    }

    #[test]
    fn websocket_handshake_warmup_is_not_part_of_rtt() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_millis(120));
            let mut websocket = tungstenite::accept(stream).unwrap();
            for _ in 0..5 {
                let message = websocket.read().unwrap();
                let Message::Text(text) = message else {
                    continue;
                };
                let token = json_number(&text, "timestamp").unwrap() as u64;
                websocket
                    .send(Message::Text(
                        format!(
                            "{{\"type\":\"pong\",\"clientTime\":{token},\"serverProcessingTime\":0}}"
                        )
                        .into(),
                    ))
                    .unwrap();
            }
        });
        let endpoint = format!("ws://{address}/ws");
        let mut engine = TransportProbeEngine::new(
            TransportProbeBackend::WebSocket,
            endpoint,
            RouteBinding::default(),
            Duration::from_secs(2),
        )
        .unwrap();
        let sample = engine.probe().unwrap();
        assert!(sample.rtt_ms < 80.0, "RTT included handshake: {sample:?}");
        assert_eq!(sample.raw_samples_ms.len(), 4);
        server.join().unwrap();
    }

    #[test]
    fn persistent_http_is_https_only_in_the_engine() {
        let error = TransportProbeEngine::new(
            TransportProbeBackend::PersistentHttp,
            "http://127.0.0.1/ping".to_string(),
            RouteBinding::default(),
            Duration::from_secs(1),
        )
        .err()
        .expect("plaintext persistent HTTP must be rejected");
        assert!(error.contains("requires https://"));
    }

    #[test]
    fn http_response_requires_unambiguous_bounded_framing_and_exact_200() {
        let valid = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\n{}";
        let response = read_http_response(&mut Cursor::new(valid)).unwrap();
        assert_eq!(response.body, b"{}");

        for invalid in [
            "HTTP/1.0 200 OK\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 2000 Weird\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 201 Created\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 200 OK\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nTransfer-Encoding: identity\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            "HTTP/1.1 200 OK\r\nBad Name: value\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\nextra",
        ] {
            assert!(
                read_http_response(&mut Cursor::new(invalid.as_bytes())).is_err(),
                "accepted ambiguous response: {invalid:?}"
            );
        }

        let oversized = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            MAX_HTTP_TOTAL_BYTES,
            "x".repeat(MAX_HTTP_TOTAL_BYTES)
        );
        assert!(read_http_response(&mut Cursor::new(oversized.into_bytes())).is_err());
    }

    #[test]
    fn absolute_io_deadline_stops_a_drip_feed() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            for _ in 0..10 {
                if stream.write_all(b"x").is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(25));
            }
        });
        let stream = TcpStream::connect(address).unwrap();
        let deadline = ProbeDeadline::after(Duration::from_millis(90)).unwrap();
        let mut stream = DeadlineTcpStream::new(stream, DeadlineControl::new(deadline));
        let started = Instant::now();
        let mut data = Vec::new();
        let error = stream.read_to_end(&mut data).unwrap_err();
        let elapsed = started.elapsed();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(elapsed < Duration::from_millis(180), "elapsed={elapsed:?}");
        drop(stream);
        server.join().unwrap();
    }

    #[test]
    fn connect_uses_remaining_budget_across_multiple_addresses() {
        let closed = TcpListener::bind("127.0.0.1:0").unwrap();
        let closed_address = closed.local_addr().unwrap();
        drop(closed);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let live_address = listener.local_addr().unwrap();
        let server = thread::spawn(move || listener.accept().unwrap());
        let deadline = ProbeDeadline::after(Duration::from_millis(300)).unwrap();
        let (stream, _) = connect_route_aware(
            &[closed_address, live_address],
            &RouteBinding::default(),
            deadline,
            DeadlineControl::new(deadline),
            true,
        )
        .unwrap();
        assert!(deadline.ensure("multi-address test").is_ok());
        drop(stream);
        let _ = server.join().unwrap();
    }

    #[test]
    fn resolver_result_is_cached_and_does_not_spawn_again() {
        let address = "127.0.0.1:443".parse().unwrap();
        let mut resolver = Resolver::from_addresses("fixture", vec![address]);
        let first = resolver
            .resolve(ProbeDeadline::after(Duration::from_secs(1)).unwrap())
            .unwrap();
        let second = resolver
            .resolve(ProbeDeadline::after(Duration::from_secs(1)).unwrap())
            .unwrap();
        assert_eq!(first, second);
        assert!(resolver.pending.is_none());
    }

    #[test]
    fn websocket_irrelevant_message_budget_is_bounded() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut websocket = tungstenite::accept(stream).unwrap();
            let Message::Text(warmup) = websocket.read().unwrap() else {
                return;
            };
            let token = json_number(&warmup, "timestamp").unwrap() as u64;
            websocket
                .send(Message::Text(
                    format!("{{\"clientTime\":{token},\"serverProcessingTime\":0}}").into(),
                ))
                .unwrap();
            let _ = websocket.read();
            for wrong in 0..=MAX_WS_IRRELEVANT_MESSAGES {
                if websocket
                    .send(Message::Text(
                        format!("{{\"clientTime\":{},\"padding\":\"x\"}}", wrong + 10_000).into(),
                    ))
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut engine = TransportProbeEngine::new(
            TransportProbeBackend::WebSocket,
            format!("ws://{address}/ws"),
            RouteBinding::default(),
            Duration::from_secs(1),
        )
        .unwrap();
        let error = engine.probe().unwrap_err();
        assert!(error.contains("irrelevant-message budget"), "{error}");
        assert!(engine.websocket.as_ref().unwrap().stream.is_none());
        drop(engine);
        server.join().unwrap();
    }

    #[test]
    fn websocket_frame_size_is_bounded() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut websocket = tungstenite::accept(stream).unwrap();
            let Message::Text(warmup) = websocket.read().unwrap() else {
                return;
            };
            let token = json_number(&warmup, "timestamp").unwrap() as u64;
            websocket
                .send(Message::Text(
                    format!("{{\"clientTime\":{token},\"serverProcessingTime\":0}}").into(),
                ))
                .unwrap();
            let _ = websocket.read();
            let _ = websocket.send(Message::Binary(vec![0; MAX_WS_FRAME_BYTES + 1].into()));
        });
        let mut engine = TransportProbeEngine::new(
            TransportProbeBackend::WebSocket,
            format!("ws://{address}/ws"),
            RouteBinding::default(),
            Duration::from_secs(1),
        )
        .unwrap();
        let error = engine.probe().unwrap_err();
        assert!(error.contains("WebSocket pong read failed"), "{error}");
        assert!(engine.websocket.as_ref().unwrap().stream.is_none());
        drop(engine);
        server.join().unwrap();
    }

    #[test]
    fn websocket_absolute_deadline_covers_warmup_and_batch() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut websocket = tungstenite::accept(stream).unwrap();
            while let Ok(Message::Text(text)) = websocket.read() {
                thread::sleep(Duration::from_millis(45));
                let token = json_number(&text, "timestamp").unwrap() as u64;
                if websocket
                    .send(Message::Text(
                        format!("{{\"clientTime\":{token},\"serverProcessingTime\":0}}").into(),
                    ))
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut engine = TransportProbeEngine::new(
            TransportProbeBackend::WebSocket,
            format!("ws://{address}/ws"),
            RouteBinding::default(),
            Duration::from_millis(170),
        )
        .unwrap();
        let started = Instant::now();
        assert!(engine.probe().is_err());
        let elapsed = started.elapsed();
        assert!(elapsed < Duration::from_millis(300), "elapsed={elapsed:?}");
        assert!(engine.websocket.as_ref().unwrap().stream.is_none());
        drop(engine);
        server.join().unwrap();
    }

    #[test]
    fn failed_websocket_warmup_never_publishes_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut websocket = tungstenite::accept(stream).unwrap();
            let _ = websocket.close(None);
        });
        let mut engine = TransportProbeEngine::new(
            TransportProbeBackend::WebSocket,
            format!("ws://{address}/ws"),
            RouteBinding::default(),
            Duration::from_millis(300),
        )
        .unwrap();
        assert!(engine.probe().is_err());
        assert!(engine.websocket.as_ref().unwrap().stream.is_none());
        drop(engine);
        server.join().unwrap();
    }
}
