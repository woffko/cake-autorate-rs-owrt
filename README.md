# cake-autorate-rs

OpenWrt package bundle for a Rust prototype of `cake-autorate` with a LuCI UI and UCI configuration.

## Project roots and acknowledgements

This project is a Rust/OpenWrt adaptation of
[`cake-autorate`](https://github.com/lynxthecat/cake-autorate), which was
created by [lynxthecat](https://github.com/lynxthecat) in 2021 and subsequently
developed by its community. The fast load-and-delay controller, terminology,
configuration model, and many defaults here deliberately follow that original
work. Development of this port started from the maintained
[`woffko/cake-autorate`](https://github.com/woffko/cake-autorate) fork.

Our sincere thanks go to lynxthecat for founding `cake-autorate`, to all of its
contributors for refining the algorithm in real networks, and to the CAKE,
OpenWrt, and `sqm-scripts` developers whose work provides the queueing and
shaping foundation. This port adds a Rust daemon, UCI/procd integration, a
combined LuCI/SQM interface, bounded ceiling probes, and RAM-only graphs; it
does not claim authorship of the original cake-autorate concept.

## Documentation

- [Controller mathematics](ALGORITHM_MATH.md) describes rate measurement,
  delay baselines, bufferbloat detection, the fast rate controller, and the
  bounded adaptive-ceiling state machine with formulas and examples.
- [Testing and observed results](TESTING.md) documents repeatable validation
  procedures and anonymized fixed-SQM, autorate, and unshaped measurements.
- [Bounded probe ceiling](ADAPTIVE_CEILING.md) is the concise state-machine and
  safety-invariant reference for the optional outer controller.
- [Full Auto-Tune](AUTOTUNE.md) documents the experimental calibration job,
  proposal formulas, safety contract, confidence score, and remaining shaped
  validation gate.
- [Transport-aware quality control](TRANSPORT_QUALITY.md) documents HTTP/TCP
  latency fusion, the strict control signal, LibreQoS-like detected ratings,
  the throughput floor, bounded natural-load search, and scheduled Full
  Auto-Tune.
- [Multi-WAN routing and lifecycle](MULTIWAN.md) describes the structured
  `main`/`mwan3` route model, per-uplink state isolation, failover/recovery,
  route identity checks, SQM ownership, and operational diagnostics.

The current targets are OpenWrt 25.12.5 on `x86/64` and
`rockchip/armv8` (`aarch64_generic`, including the Banana Pi R2 Pro). Native
route-bound WebSocket/TCP/HTTP probing uses statically linked Rust TLS and
socket libraries; ordinary OpenWrt runtime dependencies remain explicit below.

## Current package tree

The current tree builds these OpenWrt 25.12.5 APKs:

- `cake-autorate-rs-1.0_rc8-r1-x86_64.apk` — x86_64 autorate daemon.
- `cake-autorate-rs-1.0_rc8-r1-aarch64_generic.apk` — rockchip/armv8
  autorate daemon.
- `luci-app-cake-autorate-rs-1.0_rc8-r1.apk` — architecture-independent LuCI
  interface and SQM integration.

The daemon package installs `uci`, `fping`, and `uclient-fetch` as dependencies.
The LuCI package installs the daemon, `luci-base`, and `sqm-scripts`; the latter
brings the CAKE, IFB, `tc`, and `ip` runtime pieces. Native RC8 transport probes
use the daemon's statically linked rustls/webpki stack. Full Auto-Tune and the
diagnostic legacy HTTP backend still use OpenWrt `uclient-fetch`, so the offline
installer includes the standard mbedTLS provider when none is already present.
The wizard now labels a device with its logical OpenWrt networks, for example
`eth1 — wan, wan6`, while continuing to save and use the physical device name.

RC6 added structured nftables `mwan3` support. One autorate instance owns one
uplink, one CAKE/IFB pair, one route identity, and independent latency,
transport, quality, throughput-reference, and adaptive-ceiling state. ICMP,
HTTP/TCP, speed tests, and Full Auto-Tune all use the same `main` route or a
validated `mwan3 use <member> exec ...` argv path without shell evaluation.
Status exposes `ACTIVE`, `STANDBY`, `OFFLINE`, or `LEARNING`, member/device,
source address, external address, fwmark, routing table, and a stable error
reason. Route/address changes discard only that uplink's learned baselines.

The Multi-WAN wizard discovers nftables mwan3 members and labels each logical,
L3, and physical path, for example `wan → pppoe-wan → eth2`. It can create one
isolated instance and SQM queue per member and rejects duplicate shaper
ownership. A speed test or Full Auto-Tune pauses only the selected daemon and
queue, verifies route identity/external IP before accepting evidence, and pins
all speedtest-go calibration phases to the first selected server.

RC8 replaces process-timed HTTP latency with a native route-bound network RTT
engine. Persistent WebSocket is the default and LibreQoS-compatible method;
TCP-connect and persistent HTTP are comparison fallbacks. DNS, process startup,
TCP/TLS, and protocol handshakes are outside the scored clock. Four raw RTT
observations are filtered per batch, every rating needs at least 20 idle and 20
loaded samples, and download/upload have independent windows. Status displays
`CURRENT` plus the retained `PREVIOUS` result; a one-direction result is labeled
`PARTIAL`, never presented as a final connection grade.

Transport measurement/rating and transport-driven CAKE control are now separate
options. Measurement can remain enabled for Status and Graphs while
`transport_controller_enabled=0` guarantees it cannot change a shaper. The
controller is disabled by default, requires a trusted native backend, rejects
route/load-phase/CPU-contaminated evidence, and needs two confirmed bad windows
before a bounded directional response. See
[TRANSPORT_QUALITY.md](TRANSPORT_QUALITY.md) for the exact contract and the RC7
measurement failure that RC8 removes.

Optional RAM-only graphs now use one configurable global budget shared across
enabled instances. LuCI derives a safe upper bound from `MemAvailable`, offers
only proportional presets (from 256 KiB through 100 MiB), pages older samples
instead of loading the whole history into the browser, and pauses history under
critical memory pressure. WAN cards are stacked vertically; synchronized
RTT/CPU and DL/UL charts retain exact hover values and fixed Y-axis labels while
their common timeline scrolls. CPU remains available in live Status without CPU
log output.

The release includes separate minimal x86_64 and rockchip/armv8 offline
bundles. Extract the matching archive under `/root/` and run its included
installer; it validates the OpenWrt release and APK architecture, backs up the
existing UCI configuration, and installs the two project APKs together with all
65 project/runtime packages from the local repository without network access.

## Repository Layout

This repository is organized as an OpenWrt package feed/SDK overlay. Each package directory follows the OpenWrt package documentation layout:

```text
package/<package-name>/Makefile
package/<package-name>/files/
package/<package-name>/src/
```

`files/` contains installed default config, init scripts, LuCI menu/ACL files, and LuCI views. `src/` contains bundled application source; OpenWrt explicitly supports bundled source code inside a package directory, commonly under `src/`.

## Contents

- `package/cake-autorate-rs` - Rust daemon package.
- `package/luci-app-cake-autorate-rs` - LuCI app for configuration and status.
- `/etc/config/cake-autorate` - UCI config installed by the daemon package.
- `/etc/init.d/cake-autorate` - procd service wrapper.
- `/usr/sbin/cake-autorated` - daemon binary.

## Current State

This is an MVP/prototype, not full feature parity with upstream bash `cake-autorate` yet.

Implemented:

- UCI-based config loading.
- Multiple enabled UCI sections via procd instances.
- Structured `route_mode=auto|main|mwan3` and `mwan3_member` routing. Native
  nftables mwan3 state is validated before a member is used; each instance
  publishes its resolved member, L3 device, source address, external address,
  fwmark and routing table. Policy failover produces independent
  `ACTIVE`/`STANDBY`/`OFFLINE`/`LEARNING` lifecycle transitions without sharing
  learned state between uplinks.
- `fping` RTT reflector probing, `fping-ts` and `tsping` ICMP timestamp OWD
  probing, explicit-server `irtt` OWD probing, plus a basic
  per-reflector `pinger_method=ping` fallback.
- Active reflector health tracking and replacement for running `fping`,
  `fping-ts`, `tsping`, `irtt`, and `ping` probes: response-deadline offences,
  baseline/EWMA comparison, periodic replacement, optional reflector stats
  logging, and pinger restart with the next spare candidate.
- Runtime status JSON and LuCI status page expose active, spare, and bad
  reflector sets plus per-reflector samples, offence counters, and last RTT.
- sysfs RX/TX byte counter sampling.
- CPU usage sampling from `/proc/stat` is always exposed in runtime status;
  `output_cpu_stats` and `output_cpu_raw_stats` control log records only.
- adaptive rate calculations using delay/load windows.
- Optional Rust-only bounded-probe ceiling extension, disabled by default so
  the upstream configured maximum remains a hard limit. When enabled, each
  direction independently qualifies clean high load, briefly tests a higher
  ceiling, promotes a clean target to its learned-safe bound, and remembers the
  lowest target that caused confirmed bufferbloat. Later probes use the midpoint
  between safe and failed bounds. Short load/delay-classification fluctuations
  are tolerated, while sustained loss or a global probe-response gap rolls back
  without poisoning the safe/failed bounds; a stall resets runtime learning.
  Absolute DL/UL caps remain hard safety limits, and UCI is never rewritten.
  Status exposes the phase, safe ceiling, failed bound, probe target, and last
  transition reason. See [ADAPTIVE_CEILING.md](ADAPTIVE_CEILING.md) for the
  state machine and acceptance tests.
- Optional native transport RTT measurement, disabled by default. Persistent
  WebSocket, TCP-connect, and persistent HTTP resolve DNS outside the timer and
  bind sockets to the selected device/source/fwmark. Measurement supplies the
  observational LibreQoS-compatible detected rating. A separate, default-off
  controller toggle may use only trusted, route-verified, CPU-clean evidence to
  block unsafe ceiling growth or run a bounded natural-traffic search above a
  protected per-direction floor. See
  [TRANSPORT_QUALITY.md](TRANSPORT_QUALITY.md).
- `tc qdisc change ... cake bandwidth ...` shaper updates.
- Upstream-style idle/stall handling: sustained idle can stop pingers, activity
  restarts them, and optional minimum-rate enforcement applies on sustained idle
  or global no-response timeout.
- daemon log rotation by age/size with best-effort gzip compression.
- JSON status file under `/var/run/cake-autorate/<instance>/status.json`.
- Optional per-instance LuCI `Graphs` history for RTT, transport/effective
  latency, total CPU, download/upload traffic, DL/UL safety floors, and detected
  grade events. It is disabled by default and enabled directly on each active
  instance card. A per-instance dropdown selects 1, 2, 5, 10, 15, 30, or 60
  second sampling. Each uplink is a separate vertical card; both charts share a
  horizontally scrollable timeline, auto-follow new samples until the user
  scrolls back, and expose exact values on hover. Samples stay only in
  `/var/run` tmpfs. A configurable global `auto` or 256 KiB–100 MiB budget is
  divided across enabled instances, dynamically capped from `MemAvailable`, and
  compacted in a streaming pass. Older rows are read in bounded pages, critical
  memory pressure pauses history, and no sample is written to router flash.
- LuCI Status can export a diagnostic text bundle containing redacted
  cake-autorate config, SQM config, runtime status, daemon logs, package
  versions, and recent syslog lines.
- LuCI Status shows the exact installed daemon and LuCI package versions at the
  top of the page.
- LuCI settings page with compact instance rows and modal tabs for detailed settings.
- LuCI cross-field validation for manual min/base/max rates, explicit
  download/upload interface conflicts, `ping` fallback pinger count, and
  duplicate managed SQM section ownership.
- LuCI and init guard against enabling an automatic IFB download interface
  without an enabled SQM backing queue for that instance. A stray IFB created by
  another SQM section does not satisfy the guard.
- Managed SQM owns its target interface exclusively: the init script disables
  conflicting unmanaged SQM queues on the same device. On systems running the
  OpenWrt `bridger` accelerator, managed SQM devices are added to its blacklist
  and an empty conflicting `clsact` is removed before SQM starts. Autorate now
  also requires a real ingress redirect to its IFB, so a failed download shaper
  cannot silently report all visible traffic in the upload direction.
- LuCI setup wizard for creating instances, importing SQM rates, running a
  router-side speed test, and writing derived limits. Its normal speed-test step
  shows only rates and the test action; backend/package/headroom controls and
  reflector scanning are available behind `Advanced test options`. A visual
  three-step navigator (`Interface`, `Speed test`, `Review`) also supports
  direct validated navigation by clicking any numbered step.
- Experimental `Full Auto-Tune` creation mode alongside the manual wizard. It
  performs interface/route/backend preflight, reflector selection, idle ICMP
  and TCP/HTTPS latency baselines, and two unshaped throughput samples on a
  reused validated server. A pure Rust calculator derives explicit DL/UL min/base/max, activity
  and delay thresholds, link-layer overhead, and bounded adaptive-ceiling
  limits. LuCI shows the raw evidence and complete proposal before creating the
  instance; job state stays under `/tmp`, cancellation terminates the process
  group, and UCI is not written before confirmation. The job also temporarily
  validates the proposed base rates with CAKE on the same server while sampling
  independent loaded ICMP RTT/loss, TCP/HTTPS request latency, and CPU, then
  restores the previous qdisc/SQM state. The TCP signal prevents prioritized
  ICMP on mobile networks from hiding real loaded latency. Server, address,
  route, telemetry, and score failures stop without a proposal. A borderline
  score receives one bounded base-rate correction and retest; a second failure
  stops. It remains experimental until both target-router gates are complete.
- Optional scheduled Full Auto-Tune, disabled by default, adds a quiet-time
  gate, maintenance window, interval, RAM-only daily byte budget, and explicit
  review-only versus validated auto-apply mode.
- LuCI instance editing keeps advanced speed test backend controls and
  pinger/reflector planning behind the advanced settings toggle. The automatic
  interface preset, speed-test headroom, and manual min/base/max escape hatches
  are also hidden from basic setup. Basic speed test actions still use the
  current unsaved interface and backend selections when those controls are
  available.
- When `Manual rate limits` is enabled, editing the SQM download/upload rates
  does not overwrite the explicit autorate min/base/max values. Automatic mode
  continues to derive base/max from SQM rates and minimums at half-rate.
- In the LuCI edit modal, enabling the basic `Enable SQM` toggle also enables
  `Manage SQM` for that instance so the setup page can recover disabled
  external/imported SQM queues without visiting advanced settings.
- `Manage SQM` defaults on to match the init-script default, and detailed SQM
  queue/link-layer fields are hidden when the instance is not managing SQM.
- Required LuCI value/list fields use packaged defaults when older/incomplete
  sections lack a key, while optional fields remain optional and empty.
- LuCI Reflectors tab can check pinger backend availability and scan configured
  reflectors plus the upstream default anycast reflector pool, including RTT and
  ICMP timestamp capability, without adding hard dependencies. It shows RTT/OWD
  backend mode, install/manual-action hints, and can run `apk add fping` or
  `apk add irtt` for supported optional backends if they are missing.
- LuCI can apply the pinger planner recommendation into pending changes for an
  existing instance, and the create wizard writes pinger method, active pinger
  count, and reflector list for new instances.
- LuCI setup tab keeps the normal path to target interface, SQM enable,
  download/upload rates, and one-click speed testing. Explicit upstream
  min/base/max controls remain available in advanced manual-rate mode.
- Basic setup uses one `Enable autorate` control for both autorate and its
  managed SQM queue. Advanced users can disable `Manage SQM` only when they
  maintain a separate enabled SQM queue themselves.
- Router-side speed test helper with optional backend autodetection:
  `librespeed-cli`, `speedtest-go`, configured `iperf3`, then built-in HTTP
  fallback. Long-running tests are executed as a short-lived LuCI job and
  polled by the browser, so they are not killed by rpcd's command timeout.
  `speedtest-go` automatically tries nearby servers, rejects an implausibly
  asymmetric automatic result, and caches the first validated server per
  instance; entering a server ID pins the test to that Ookla server. Optional
  backend packages are not hard dependencies.
- Disabled instances are shown as `DISABLED` in LuCI and do not display stale
  runtime counters; the init script removes stale status samples after a
  service stop.
- Integrated SQM backend sync: each `cake-autorate` UCI section can own a matching
  `sqm` queue section.
- Optional MQTT publisher service: per-instance MQTT export reads daemon
  SUMMARY/CPU log records, publishes state via `mosquitto_pub`, and registers
  Home Assistant discovery sensors when enabled.
- Automatic interface preset: selecting the target interface fills
  `sqm_interface`, `ul_if`, `dl_if=ifb4<target>`, and empty/generated
  `ping_extra_args=-I <target>` for non-IRTT pingers so reflector probes are
  bound to the selected uplink by default.
- Automatic SQM rate import from an existing `/etc/config/sqm` queue for the
  selected interface when available.
- Upstream-style max-wire packet compensation for OWD thresholds and achieved
  rate monitor timing, using live interface MTU plus CAKE `atm/noatm overhead`
  from `tc qdisc show`.
- Upstream-style stale reflector response guard: pinger samples processed more
  than 500 ms after their timestamp are logged and skipped.
- LuCI status page with start, restart, stop actions. An enabled instance that
  has received no valid probe sample after ten seconds shows a compact
  `No probe replies` warning with pinger/multi-WAN routing guidance.

Known limits:

- Adaptive ceiling is intentionally not part of upstream `cake-autorate` and is
  an explicit opt-in. Configure absolute caps deliberately; leaving it off
  preserves exact upstream hard-max semantics. The recommended initial tuning
  is 20 seconds qualification, a 3% open probe, 8 seconds observation, 30
  seconds cooldown, and 900 seconds failed-bound memory. Runtime status/logs
  expose all phase transitions and effective-ceiling changes.
- `pinger_method=ping` starts one basic ping process per active reflector, but
  it remains a fallback; use `fping`, `fping-ts`, `tsping`, or explicit-server
  `irtt` where those backends are available.
- `pinger_method=irtt` requires the optional `irtt` package and at least one
  explicit `list irtt_server ...` entry. Generic DNS reflector pools are not
  used as IRTT servers. The router and IRTT servers also need synchronized
  clocks; upstream-compatible parsing ignores negative one-way delays from
  unsynchronized hosts.
- `ping_prefix_string` remains available only for compatible legacy/main-route
  setups and is always tokenized without a shell. The init script migrates the
  exact legacy form `mwan3 use <member> exec` to structured `route_mode=mwan3`;
  structured Multi-WAN never accepts a free-form shell prefix.
- The LuCI wizard and interface preset fill `ping_extra_args=-I <target>` when
  the field is empty or still contains a generated `-I ...` value. Manual
  multi-argument ping args and `ping_prefix_string` are preserved.
- `fping-ts` and `tsping` depend on reflectors that answer ICMP timestamp
  probes; many public DNS anycast reflectors do not.
- `tsping` is runtime-detected and not a hard package dependency; install a
  compatible `tsping` binary manually where available before selecting it.
- reflector health/replacement is implemented as an MVP; `fping-ts` uses
  separate DL/UL OWD samples while RTT backends still use RTT/2 estimates.
- The LuCI planner can scan a broader upstream default candidate pool and apply
  the recommended pinger method, active count, and ordered reflector list.
- Use the external LibreQoS Internet Quality Test at https://test.libreqos.com/
  as a manual browser-side validation tool after configuring autorate. It is
  intentionally documented only, not integrated into the wizard or router-side
  speed test backend.
- Pinger auto-install is intentionally limited: the GUI can install/repair the
  supported `fping` package used by `fping`/`fping-ts` and the optional `irtt`
  package. `tsping` remains a manual binary install, and `irtt` is only ready
  when explicit IRTT servers are configured and clocks are synchronized.
- Multi-WAN policy definitions, tracking targets, weights, metrics, and the
  underlying network interfaces remain router/network configuration. Full
  per-uplink integration requires the native nftables mwan3 backend and its
  member-scoped status API. The application validates and consumes that state;
  it does not invent a missing uplink or repair an invalid mwan3 policy.
- MQTT is an optional sidecar service rather than daemon core. It requires a
  configured broker, `log_to_file=1`, `output_summary_stats=1`, and
  `mosquitto_pub` from either `mosquitto-client-nossl` or
  `mosquitto-client-ssl`. CPU sensors additionally require
  `output_cpu_stats=1`.

SQM integration:

- `luci-app-cake-autorate-rs` is intended to be the single LuCI UI for SQM setup
  plus autorate control.
- Installing the LuCI package automatically installs `sqm-scripts` and
  `uclient-fetch`, which provide the normal OpenWrt CAKE/IFB shaping stack and
  the Full Auto-Tune TCP/HTTPS latency gate.
- The LuCI package declares `PROVIDES:=luci-app-sqm` and `CONFLICTS:=luci-app-sqm`
  as the replacement intent. OpenWrt 25.12 APK package generation currently emits
  the provide metadata, but conflict metadata still needs verification in final
  packages.
- The UI includes the required `luci-app-sqm` settings: enable flag, interface,
  download/upload rates, debug logging, verbosity, qdisc, queue setup script,
  DSCP/ECN options, queue limits, latency targets, raw qdisc options, link layer
  mode, overhead, and advanced link layer parameters.
- `cake-autorate` UCI sections are the user-facing source of truth; the init
  script synchronizes matching `sqm` queue sections before starting SQM and
  autorate.
- Stopping `cake-autorate` also stops SQM runtime state for sections marked as
  managed by cake-autorate, leaving unrelated SQM queues alone.
- Disabled sections and sections with `manage_sqm=0` do not mirror into SQM;
  stale owned SQM sections are cleaned up instead.
- Multiple interface/queue pairs are represented as multiple `cake_autorate`
  sections and shown in one compact LuCI grid.

## Runtime Dependencies

Required package dependencies:

- `uci`
- `fping`
- `uclient-fetch`

LuCI package dependencies:

- `cake-autorate-rs`
- `luci-base`
- `sqm-scripts`

Native WebSocket and persistent-HTTP probes use statically linked rustls and
webpki roots and add no dynamic APK dependency. Full Auto-Tune and
`legacy-http` use `uclient-fetch`; normal LuCI images already provide a
`libustream` TLS provider and CA certificates. The offline bundle contains
`ca-bundle` and the default `libustream-mbedtls20201210`, while preserving an
already installed OpenSSL or wolfSSL provider.

`sqm-scripts` pulls the required `tc`, CAKE, IFB, iptables, and related shaping
packages on OpenWrt.

Optional speed test backend packages:

- `librespeed-cli`
- `speedtest-go`
- `iperf3`
- `jsonfilter` is required to parse CLI backend JSON and is installed by the
  LuCI helper when installing an optional backend.

The LuCI Speed Test tab and setup wizard show backend availability and can run
`apk add` for the selected optional backend. The built-in HTTP backend requires
`curl`, `uclient-fetch`, or `wget`; it does not require an extra speed test
package.

Optional pinger backend binaries:

- `fping` with `--icmp-timestamp` support for `pinger_method=fping-ts`
- `tsping`
- `irtt` for `pinger_method=irtt`, with explicit `irtt_server` entries

The daemon accepts `pinger_method=tsping` when the binary is present in PATH and
`pinger_method=irtt` when `irtt` is installed and explicit IRTT servers are
configured. IRTT OWD samples require the router and IRTT server clocks to be
synchronized; negative one-way delays are ignored to match upstream behavior.
`tsping` remains optional because no supported OpenWrt package was available on
the current test router. The advanced LuCI Reflectors tab can check pinger
backend availability, show RTT/OWD and round-robin/individual mode, install
supported `fping`/`irtt` packages when needed, scan reflectors, and apply the
recommendation into pending changes. If `tsping` is manually installed, the
planner can use it as the timestamp probe path when `fping --icmp-timestamp` is
unavailable. The create wizard writes pinger defaults and can run the same scan
before creating a new instance.

Optional MQTT client packages:

- `mosquitto-client-nossl`
- `mosquitto-client-ssl`

The LuCI Logging tab shows MQTT availability and can install the default
`mosquitto-client-nossl` package. After setting `mqtt_enabled=1` and
`mqtt_host`, enable and start `/etc/init.d/cake-autorate-mqtt`; the publisher
creates Home Assistant discovery sensors and publishes instance state under the
configured base topic.

## Build In OpenWrt SDK

Use a clean OpenWrt 25.12.5 SDK for either `x86/64` or `rockchip/armv8`. The
Rust feed builds a large host Rust/LLVM toolchain on first use, so cache the SDK
or use a prepared build image for normal iteration.

Recommended feed workflow:

```sh
cd /path/to/openwrt-sdk
cp feeds.conf.default feeds.conf
cat /path/to/cake-autorate-rs/feeds.conf.example >> feeds.conf
./scripts/feeds update packages luci
./scripts/feeds update cake_autorate_rs
./scripts/feeds install rust fping luci-base
./scripts/feeds install cake-autorate-rs luci-app-cake-autorate-rs
make defconfig
make package/cake-autorate-rs/compile V=s -j1
make package/luci-app-cake-autorate-rs/compile V=s -j1
```

Overlay workflow during local development:

```sh
cp -a package/cake-autorate-rs /path/to/openwrt-sdk/package/
cp -a package/luci-app-cake-autorate-rs /path/to/openwrt-sdk/package/
```

Enable packages in `.config` when building as modules:

```text
CONFIG_PACKAGE_cake-autorate-rs=m
CONFIG_PACKAGE_luci-app-cake-autorate-rs=m
CONFIG_PACKAGE_fping=m
CONFIG_PACKAGE_rust=m
```

## Install

Copy the matching daemon APK plus the noarch LuCI APK to the router and install
them together. For x86_64:

```sh
apk add --allow-untrusted \
  /root/cake-autorate-rs-1.0_rc8-r1-x86_64.apk \
  /root/luci-app-cake-autorate-rs-1.0_rc8-r1.apk
```

For rockchip/armv8 (`aarch64_generic`):

```sh
apk add --allow-untrusted \
  /root/cake-autorate-rs-1.0_rc8-r1-aarch64_generic.apk \
  /root/luci-app-cake-autorate-rs-1.0_rc8-r1.apk
```

`fping` and `sqm-scripts` are pulled automatically. Optional pinger backends:

```sh
# fping-ts uses the installed fping binary; no extra package is required
apk add irtt  # also configure explicit IRTT servers and synchronized clocks
# tsping is a compatible binary installed manually; ping is supplied by the base system
```

### Offline install bundle

For a router without access to the package feeds, copy the matching release
bundle to `/root/`, then run one of these platform-specific pairs.

x86_64:

```sh
cd /root
tar -xzf cake-autorate-rs-1.0-rc8-openwrt-25.12.5-x86_64-offline-bundle.tar.gz
/root/install-cake-autorate-rs-1.0-rc8-x86_64.sh
```

Banana Pi R2 Pro and other OpenWrt 25.12.5 rockchip/armv8 devices:

```sh
cd /root
tar -xzf cake-autorate-rs-1.0-rc8-openwrt-25.12.5-rockchip-armv8-offline-bundle.tar.gz
/root/install-cake-autorate-rs-1.0-rc8-aarch64_generic.sh
```

The installer resolves its own location, so it also works when the extracted
bundle is kept in another directory.

Each archive is about 3.3 MiB and needs roughly 8 MiB of free space while
both the archive and its extracted contents are present. If `/root/` is too
small, use another writable filesystem (for example `/tmp/` when its tmpfs has
enough RAM) for both commands instead.

Optional speed test backends can be installed from LuCI or manually:

```sh
apk add librespeed-cli speedtest-go iperf3 jsonfilter
```

Optional MQTT support can be installed from LuCI or manually:

```sh
apk add mosquitto-client-nossl
```

Then edit `/etc/config/cake-autorate` or use LuCI:

```sh
uci set cake-autorate.primary.enabled='1'
uci commit cake-autorate
/etc/init.d/cake-autorate enable
/etc/init.d/cake-autorate restart
```

Graph history is opt-in per instance. The same switch is available on the
LuCI `Graphs` page; from the shell it can be changed with:

```sh
uci set cake-autorate.primary.graph_history_enabled='1'  # use '0' to disable
uci set cake-autorate.primary.graph_history_interval_s='10'  # accepted: 1-60
uci set cake-autorate.globals.graph_history_ram_budget_kib='auto'
uci commit cake-autorate
/etc/init.d/cake-autorate restart
```

When enabled, `history.csv` is sampled at the selected interval under
`/var/run/cake-autorate/<instance>/`. Each row contains timestamp, RTT,
transport/effective latency, total CPU, download/upload kbit/s, safety floors,
and a detected-grade event when one changes. The global budget accepts `auto`
or one of `256`, `512`, `1024`, `2048`, `4096`, `8192`, `16384`, `32768`,
`65536`, and `102400` KiB. The daemon caps that request according to available
RAM and divides the effective total across enabled histories. For example,
roughly 100 MiB available permits at most 1 MiB total, while 1 GiB permits at
most 100 MiB. At less than 16 MiB available, collection pauses and releases its
history. Files are removed on service stop/reboot and never stored in flash.

## Quick Checks

```sh
cake-autorated --instance primary --dump-config
cake-autorated --instance primary --once
cat /var/run/cake-autorate/primary/status.json
/usr/libexec/cake-autorate-rs/speedtest primary "" status auto
/usr/libexec/cake-autorate-rs/mqtt-status primary status
```

For a no-shaper smoke test, disable both shaper adjustment flags:

```sh
uci set cake-autorate.primary.adjust_dl_shaper_rate='0'
uci set cake-autorate.primary.adjust_ul_shaper_rate='0'
uci commit cake-autorate
cake-autorated --instance primary --once
```

For `ping` fallback and CPU/log smoke tests, use a temporary disabled-rate
instance with explicit counter paths, `adjust_dl_shaper_rate='0'`,
`adjust_ul_shaper_rate='0'`, `pinger_method='ping'`, `output_cpu_stats='1'`,
and a temporary `log_file_path_override`.

## Development Notes

Rust is a reasonable daemon language for this project because it provides one static-ish native binary, predictable memory safety, and better long-term maintainability than a large shell daemon. The main practical cost on OpenWrt is build complexity: the first SDK build of `rust/host` is heavy because it compiles Rust/LLVM tooling.

For faster iteration, keep a cached SDK or CI artifact with the Rust host toolchain already built.
