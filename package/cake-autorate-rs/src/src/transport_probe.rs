use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::process::{Command, Stdio};
use std::sync::{Arc, Once};
use std::thread;
use std::time::{Duration, Instant};
use tungstenite::client::IntoClientRequest;
use tungstenite::http::HeaderValue;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{client_tls_with_config, Message, WebSocket};

const DEFAULT_STREAMS: usize = 4;
const STREAM_DELAY: Duration = Duration::from_millis(50);
const MAX_HTTP_HEADER_BYTES: usize = 64 * 1024;

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

    fn socket_addrs(&self) -> Result<Vec<SocketAddr>, String> {
        let mut addresses = (self.host.as_str(), self.port)
            .to_socket_addrs()
            .map_err(|error| format!("failed to resolve {}: {error}", self.host))?
            .filter(SocketAddr::is_ipv4)
            .collect::<Vec<_>>();
        addresses.sort();
        addresses.dedup();
        if addresses.is_empty() {
            return Err(format!("{} has no IPv4 address", self.host));
        }
        Ok(addresses)
    }
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
            TransportProbeBackend::PersistentHttp
                if !matches!(parsed.scheme.as_str(), "http" | "https") =>
            {
                return Err(
                    "persistent HTTP backend requires http:// or https:// endpoint".to_string(),
                )
            }
            TransportProbeBackend::LegacyHttp => {}
            _ => {}
        }
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
            websocket,
            http,
        })
    }

    pub fn probe(&mut self) -> Result<TransportProbeSample, String> {
        match self.backend {
            TransportProbeBackend::WebSocket => self
                .websocket
                .as_mut()
                .ok_or_else(|| "websocket probe is unavailable".to_string())?
                .probe(),
            TransportProbeBackend::TcpConnect => {
                probe_tcp_batch(&self.endpoint, &self.binding, self.timeout, DEFAULT_STREAMS)
            }
            TransportProbeBackend::PersistentHttp => self
                .http
                .as_mut()
                .ok_or_else(|| "persistent HTTP probe is unavailable".to_string())?
                .probe(),
            TransportProbeBackend::LegacyHttp => probe_legacy_http(&self.endpoint, self.timeout),
        }
    }
}

type WsStream = WebSocket<MaybeTlsStream<TcpStream>>;

struct WebSocketProbe {
    endpoint: String,
    parsed: ParsedEndpoint,
    binding: RouteBinding,
    timeout: Duration,
    stream: Option<WsStream>,
    sequence: u64,
}

impl WebSocketProbe {
    fn new(
        endpoint: String,
        parsed: ParsedEndpoint,
        binding: RouteBinding,
        timeout: Duration,
    ) -> Self {
        Self {
            endpoint,
            parsed,
            binding,
            timeout,
            stream: None,
            sequence: 1,
        }
    }

    fn connect(&mut self) -> Result<(), String> {
        install_crypto_provider();
        let addresses = self.parsed.socket_addrs()?;
        let (tcp, _) = connect_route_aware(&addresses, &self.binding, self.timeout, false)?;
        tcp.set_read_timeout(Some(self.timeout))
            .map_err(|error| format!("failed to set WebSocket read timeout: {error}"))?;
        tcp.set_write_timeout(Some(self.timeout))
            .map_err(|error| format!("failed to set WebSocket write timeout: {error}"))?;
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
        let (stream, _) = client_tls_with_config(request, tcp, None, None)
            .map_err(|error| format!("WebSocket handshake failed: {error}"))?;
        self.stream = Some(stream);
        self.raw_batch(1)?;
        Ok(())
    }

    fn probe(&mut self) -> Result<TransportProbeSample, String> {
        let connection_reused = self.stream.is_some();
        if self.stream.is_none() {
            self.connect()?;
        }
        match self.raw_batch(DEFAULT_STREAMS) {
            Ok((raw, server_processing)) => {
                let (rtt_ms, accepted) = robust_median(&raw)
                    .ok_or_else(|| "WebSocket returned no valid RTT samples".to_string())?;
                Ok(TransportProbeSample {
                    backend: TransportProbeBackend::WebSocket,
                    endpoint: self.endpoint.clone(),
                    rtt_ms,
                    discarded_samples: raw.len().saturating_sub(accepted.len()),
                    raw_samples_ms: accepted,
                    server_processing_ms: server_processing,
                    trusted: true,
                    connection_reused,
                })
            }
            Err(error) => {
                self.stream = None;
                Err(error)
            }
        }
    }

    fn raw_batch(&mut self, streams: usize) -> Result<(Vec<f64>, f64), String> {
        let ws = self
            .stream
            .as_mut()
            .ok_or_else(|| "WebSocket is disconnected".to_string())?;
        let mut samples = Vec::with_capacity(streams.max(1));
        let mut processing_total_ms = 0.0;
        for stream_id in 1..=streams.max(1) {
            let token = self.sequence;
            self.sequence = self.sequence.wrapping_add(1).max(1);
            let payload = format!(
                "{{\"type\":\"ping\",\"timestamp\":{token},\"streamId\":{stream_id},\"payloadSizeBytes\":0,\"payload\":\"\"}}"
            );
            let started = Instant::now();
            ws.send(Message::Text(payload.into()))
                .map_err(|error| format!("WebSocket ping send failed: {error}"))?;
            loop {
                let message = ws
                    .read()
                    .map_err(|error| format!("WebSocket pong read failed: {error}"))?;
                match message {
                    Message::Text(text) => {
                        let Some(received_token) =
                            json_number(&text, "clientTime").map(|value| value as u64)
                        else {
                            continue;
                        };
                        if received_token != token {
                            continue;
                        }
                        let processing_ms = json_number(&text, "serverProcessingTime")
                            .unwrap_or(0.0)
                            .max(0.0);
                        let rtt_ms = started.elapsed().as_secs_f64() * 1000.0 - processing_ms;
                        if rtt_ms.is_finite() && rtt_ms > 0.0 {
                            samples.push(rtt_ms);
                            processing_total_ms += processing_ms;
                        }
                        break;
                    }
                    Message::Ping(payload) => {
                        ws.send(Message::Pong(payload))
                            .map_err(|error| format!("WebSocket pong send failed: {error}"))?;
                    }
                    Message::Close(_) => return Err("WebSocket closed during probe".to_string()),
                    _ => {}
                }
            }
            if stream_id < streams {
                thread::sleep(STREAM_DELAY);
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
    timeout: Duration,
    tls_config: Arc<ClientConfig>,
    stream: Option<StreamOwned<ClientConnection, TcpStream>>,
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
        Self {
            endpoint,
            parsed,
            binding,
            timeout,
            tls_config: Arc::new(config),
            stream: None,
            sequence: 1,
        }
    }

    fn connect(&mut self) -> Result<(), String> {
        if self.parsed.scheme != "https" {
            return Err("persistent HTTP currently requires https://".to_string());
        }
        let addresses = self.parsed.socket_addrs()?;
        let (tcp, _) = connect_route_aware(&addresses, &self.binding, self.timeout, false)?;
        tcp.set_read_timeout(Some(self.timeout))
            .map_err(|error| format!("failed to set HTTP read timeout: {error}"))?;
        tcp.set_write_timeout(Some(self.timeout))
            .map_err(|error| format!("failed to set HTTP write timeout: {error}"))?;
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
        self.stream = Some(stream);
        self.raw_probe()?;
        Ok(())
    }

    fn probe(&mut self) -> Result<TransportProbeSample, String> {
        let connection_reused = self.stream.is_some();
        if self.stream.is_none() {
            self.connect()?;
        }
        let mut raw = Vec::with_capacity(DEFAULT_STREAMS);
        let mut processing_total = 0.0;
        for _ in 0..DEFAULT_STREAMS {
            match self.raw_probe() {
                Ok((rtt_ms, processing_ms)) => {
                    raw.push(rtt_ms);
                    processing_total += processing_ms;
                }
                Err(error) => {
                    self.stream = None;
                    return Err(error);
                }
            }
        }
        let (rtt_ms, accepted) = robust_median(&raw)
            .ok_or_else(|| "persistent HTTP returned no valid RTT samples".to_string())?;
        Ok(TransportProbeSample {
            backend: TransportProbeBackend::PersistentHttp,
            endpoint: self.endpoint.clone(),
            rtt_ms,
            discarded_samples: raw.len().saturating_sub(accepted.len()),
            raw_samples_ms: accepted,
            server_processing_ms: processing_total / DEFAULT_STREAMS as f64,
            trusted: true,
            connection_reused,
        })
    }

    fn raw_probe(&mut self) -> Result<(f64, f64), String> {
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| "persistent HTTP is disconnected".to_string())?;
        let separator = if self.parsed.path.contains('?') {
            '&'
        } else {
            '?'
        };
        let request = format!(
            "GET {}{}t={} HTTP/1.1\r\nHost: {}\r\nUser-Agent: cake-autorate-rs/rc8\r\nAccept: application/json\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n",
            self.parsed.path, separator, self.sequence, self.parsed.host
        );
        self.sequence = self.sequence.wrapping_add(1).max(1);
        let started = Instant::now();
        stream
            .write_all(request.as_bytes())
            .and_then(|_| stream.flush())
            .map_err(|error| format!("persistent HTTP request failed: {error}"))?;
        let headers = read_http_response(stream)?;
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        let processing_ms = server_processing_ms(&headers).unwrap_or(0.0).max(0.0);
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
    timeout: Duration,
    timed: bool,
) -> Result<(TcpStream, f64), String> {
    let source = binding.parsed_source()?;
    let mark = binding.parsed_mark()?;
    let mut errors = Vec::new();
    for address in addresses.iter().copied().filter(SocketAddr::is_ipv4) {
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
        if let Err(error) = socket.connect_timeout(&SockAddr::from(address), timeout) {
            errors.push(format!("connect {address}: {error}"));
            continue;
        }
        let connect_ms = if timed {
            started.elapsed().as_secs_f64() * 1000.0
        } else {
            0.0
        };
        let stream: TcpStream = socket.into();
        stream
            .set_nodelay(true)
            .map_err(|error| format!("failed to set TCP_NODELAY: {error}"))?;
        return Ok((stream, connect_ms));
    }
    Err(format!(
        "route-aware TCP connection failed: {}",
        errors.join("; ")
    ))
}

fn probe_tcp_batch(
    endpoint: &str,
    binding: &RouteBinding,
    timeout: Duration,
    streams: usize,
) -> Result<TransportProbeSample, String> {
    let parsed = ParsedEndpoint::parse(endpoint)?;
    let addresses = parsed.socket_addrs()?;
    let mut raw = Vec::with_capacity(streams);
    for _ in 0..streams.max(1) {
        let (_, connect_ms) = connect_route_aware(&addresses, binding, timeout, true)?;
        if connect_ms.is_finite() && connect_ms > 0.0 {
            raw.push(connect_ms);
        }
        thread::sleep(STREAM_DELAY);
    }
    let (rtt_ms, accepted) = robust_median(&raw)
        .ok_or_else(|| "TCP connect returned no valid RTT samples".to_string())?;
    Ok(TransportProbeSample {
        backend: TransportProbeBackend::TcpConnect,
        endpoint: endpoint.to_string(),
        rtt_ms,
        discarded_samples: raw.len().saturating_sub(accepted.len()),
        raw_samples_ms: accepted,
        server_processing_ms: 0.0,
        trusted: true,
        connection_reused: false,
    })
}

fn probe_legacy_http(endpoint: &str, timeout: Duration) -> Result<TransportProbeSample, String> {
    let started = Instant::now();
    let status = Command::new("uclient-fetch")
        .arg("-4")
        .arg("-q")
        .arg("-T")
        .arg(timeout.as_secs().max(1).to_string())
        .arg("-O")
        .arg("/dev/null")
        .arg(endpoint)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| format!("failed to execute legacy uclient-fetch: {error}"))?;
    if !status.success() {
        return Err(format!("legacy uclient-fetch exited with {status}"));
    }
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

fn read_http_response(
    stream: &mut StreamOwned<ClientConnection, TcpStream>,
) -> Result<String, String> {
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
        if data.len() > MAX_HTTP_HEADER_BYTES && header_end.is_none() {
            return Err("persistent HTTP response headers are too large".to_string());
        }
        if header_end.is_none() {
            header_end = find_bytes(&data, b"\r\n\r\n").map(|index| index + 4);
            if let Some(end) = header_end {
                let headers = String::from_utf8_lossy(&data[..end]);
                if !headers.starts_with("HTTP/1.1 200") && !headers.starts_with("HTTP/1.0 200") {
                    return Err(format!(
                        "persistent HTTP returned {}",
                        headers.lines().next().unwrap_or("invalid status")
                    ));
                }
                content_length = header_value(&headers, "content-length")
                    .and_then(|value| value.parse::<usize>().ok());
                if content_length.is_none()
                    && header_value(&headers, "transfer-encoding")
                        .map(|value| value.eq_ignore_ascii_case("chunked"))
                        .unwrap_or(false)
                {
                    return Err("chunked persistent HTTP response is unsupported".to_string());
                }
            }
        }
        if let Some(end) = header_end {
            let needed = content_length.unwrap_or(0);
            if data.len() >= end.saturating_add(needed) {
                return Ok(String::from_utf8_lossy(&data[..end]).to_string());
            }
        }
    }
}

fn header_value<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name).then_some(value.trim())
    })
}

fn server_processing_ms(headers: &str) -> Option<f64> {
    let value = header_value(headers, "server-timing")?;
    let marker = "dur=";
    let tail = value.get(value.find(marker)? + marker.len()..)?;
    let end = tail
        .find(|ch: char| !(ch.is_ascii_digit() || matches!(ch, '.' | '-' | '+')))
        .unwrap_or(tail.len());
    tail.get(..end)?.parse::<f64>().ok()
}

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

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn parses_server_timing_and_json_numbers() {
        assert_eq!(
            server_processing_ms("HTTP/1.1 200 OK\r\nServer-Timing: processing;dur=1.25\r\n"),
            Some(1.25)
        );
        assert_eq!(
            json_number("{\"clientTime\":42,\"x\":1}", "clientTime"),
            Some(42.0)
        );
        assert_eq!(json_number("{\"clientTime\":null}", "clientTime"), None);
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
}
