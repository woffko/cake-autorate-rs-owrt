# Features and limitations

A detailed list of what the current source implements, followed by the
runtime notes that used to live in the README. User-facing behaviour of each
screen is described in the [user guide](USER_GUIDE.md).

## Implemented features

This is the current Rust/OpenWrt release-candidate implementation, not a
drop-in replacement for every upstream Bash utility or supported platform.

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
  The Status value is whole-router utilization. Run
  `/usr/sbin/cake-autorated --cpu-profile 30` to measure the daemon,
  persistent pingers and scheduler separately, including short-lived child
  work waited by each daemon.
- adaptive rate calculations using delay/load windows.
- External/manual SQM (`manage_sqm=0`) permits bandwidth-only control of
  explicitly enabled, addressable root CAKE queues. Disabled directions and
  custom ingress steering stay untouched. Missing queues are reported as
  `WAITING_EXTERNAL_SQM`, without automatic topology repair; native Auto-Tune
  still requires its separate managed/exclusive restore authority.
- Cold latency baselines use three independent, recent low-load counter/RTT
  observations, not a fixed 100 ms starting value. Until qualified, the daemon
  holds off that reflector's control and measurement decisions. Status reports
  `latency_baseline_ready` and counts of ready/pending reflectors; initial
  qualification reports `LEARNING`. Real route changes restart qualification.
- Optional Rust-only bounded-probe ceiling extension. **Validated ceiling
  only** is the conservative default when link classification is inconclusive;
  bounded passive/scheduled learning is an explicit policy. Each direction
  starts from its exact tested-safe point, independently qualifies clean high
  load, briefly tests a higher ceiling, promotes only a clean target with a
  measurable throughput gain, and remembers the
  lowest target that caused confirmed bufferbloat. Later probes use the midpoint
  between safe and failed bounds. Short load/delay-classification fluctuations
  are tolerated, while sustained loss or a global probe-response gap rolls back
  without poisoning the safe/failed bounds. A brief stall pauses learning;
  sustained response loss past the effective global timeout resets learned
  bounds. Real route changes and daemon restarts still reset runtime learning.
  Measured-raw DL/UL caps and any tighter service caps remain hard safety
  limits, and runtime learning never rewrites UCI.
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
- Passive detected-rating load classification is independent of controller
  high/low/idle state. A bounded rolling peak for entry, average for exit,
  enter/exit hysteresis,
  direction latch, and dropout grace turn real forwarded traffic into stable
  `DL`, `UL`, or `BIDIRECTIONAL` rating phases without double-counting byte
  counters. Optional `Get rating` automatic/client capture uses the same
  detector and supplies a bounded per-direction trigger; it never bypasses
  shaping. Automatic capture first enforces a quiet window and runs separate
  download-only and upload-only load phases. The top-level grade is published
  only from a fresh finalized capture containing trusted ICMP and transport
  evidence for both directions. Partial, stale, compatibility, or one-sided
  evidence remains diagnostic and cannot replace the last complete grade.
- The controller, rating detector, transport scheduler, and RAM graph history
  reuse one atomic per-interval RX/TX counter sample. This prevents either
  direction from disappearing because another consumer already advanced the
  counter baseline.
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
- While running, each managed instance checks the actual CAKE/IFB/ingress state.
  If it disappears, probing and rating stop, Status reports the concrete
  runtime error, and the Rust service-lifecycle path performs a targeted,
  ownership-checked SQM restart through the narrow init bridge.
  Attempts are serialized, deferred during a speed test, and rate-limited to
  avoid a recovery loop.
- The mandatory Status **Services** column independently reconciles configured
  intent with daemon processes, managed SQM ownership, both CAKE qdiscs and
  rates, IFB/redirect topology, native traffic-rule attestation, current heavy
  operation, and guarded apply state. It exposes `HEALTHY`, `DISABLED`,
  `DEGRADED`, `ORPHANED`, or `BLOCKED` plus the exact component-level reason.
- Optional native profile traffic rules classify only outbound packets in the
  private `inet cake_autorate_dscp` table. Gaming, Best overall, and Fair have
  separate built-in defaults and editable ordered custom rules. No qosify,
  eBPF, external qdisc owner, or free-form shell rule is used. The loaded
  ruleset is SHA-256-attested against its instance, resolved interface, and
  profile; Status reports missing, ineffective, drifted, and orphaned rules.
- LuCI setup wizard for creating instances, importing SQM rates, running a
  router-side speed test, and writing derived limits. Its normal speed-test step
  shows only rates and the test action; backend/package/headroom controls and
  reflector scanning are available behind `Advanced test options`. A visual
  three-step navigator (`Interface`, `Speed test`, `Review`) also supports
  direct validated navigation by clicking any numbered step.
- `Full Auto-Tune` creation and re-run mode alongside the manual wizard. It
  performs interface/route/backend preflight, reflector selection, idle ICMP
  and native persistent-transport baselines, and one bidirectional plus two
  download-only and two upload-only unshaped controls on a reused validated
  server. A pure Rust calculator derives explicit
  DL/UL min/base/max, activity and delay thresholds, link-layer overhead, and
  bounded adaptive-ceiling limits. LuCI shows the raw evidence and complete
  proposal before creating the instance; job state stays under `/tmp`,
  cancellation terminates the process group, and UCI is not written before
  confirmation. The shaped job records ICMP p95-to-p95 growth, native transport
  p95-to-p95 growth, loss, aggregate/busiest-core/softirq CPU, CAKE counters,
  three distinct throughput ratios, and forwarded client background before
  restoring the previous qdisc/SQM state. Typed gates and a bounded Rust
  per-direction optimizer search the measured quality/throughput boundary,
  repeat unreliable observations, raise a candidate until its hard floor is
  reachable, and confirm the exact selected pair. The public Review contract
  carries immutable option IDs, manifest/review digests, exact tested topology
  and rates, and the option's complete acknowledgement list. Missing or
  structurally invalid evidence remains a hard stop; safe background,
  retention, latency or topology exceptions are explicit-review only. No
  acknowledgement can weaken route, SQM ownership, loss/latency, measurement,
  or runtime-restoration gates.
- Optional scheduled Full Auto-Tune, disabled by default, adds a quiet-time
  gate, maintenance window, interval, RAM-only daily byte budget, and explicit
  review-only versus validated auto-apply mode. Unattended apply requires a
  preferred option whose Auto-Apply evidence contract passes with no required
  acknowledgements, met profile objectives and complete restored runtime;
  every acknowledged option remains explicit-review only.
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
- Native router-side Speed Test with two user choices: `Auto` and
  `speedtest-go`. Both resolve to the same route-bound Rust operation and the
  Full daemon package depends on `speedtest-go`; retired librespeed, iperf3 and
  built-in HTTP execution paths are no longer selectable. The backend tries
  nearby servers, rejects an implausibly asymmetric automatic result, and
  caches the first validated server per instance; entering a server ID pins
  the test to that Ookla server. Jobs have durable identities and are polled or
  reattached by LuCI instead of living inside an rpcd request.
- Disabled instances are shown as `DISABLED` in LuCI and do not display stale
  runtime counters; the init script removes stale status samples after a
  service stop.
- Integrated SQM backend sync: each `cake-autorate` UCI section can own a matching
  `sqm` queue section.
- Optional native MQTT publisher service: per-instance MQTT export reads
  bounded SUMMARY/CPU log records, speaks MQTT 3.1.1 directly without exposing
  credentials in a child-process argument list, and registers retained Home
  Assistant discovery and availability records when enabled.
- Automatic interface preset: selecting the target interface fills
  `sqm_interface`, `ul_if`, and `dl_if=ifb4<target>`. Automatic pinger binding
  is derived from the current route at runtime, not stored in extra arguments.
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
- Native Apply, the LuCI wizard and interface presets preserve user
  `ping_extra_args` instead of generating `-I` values. Runtime uses fping's
  interface/source options, ping's interface option, tsping's `--interface`
  (and route mark for mwan3), or IRTT's `--local` source-address option.
  IRTT source binding is not an interface bind and still requires the existing
  main/mwan3 route contract. Conflicting saved pins fail explicitly; a legacy
  `-I` cannot safely be guessed to be generated rather than user-authored.
  Review/remove that old pin after changing interfaces or switching to tsping.
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
- MQTT is an optional native sidecar rather than controller authority. It
  requires a configured plain-MQTT broker, `log_to_file=1`, and
  `output_summary_stats=1`; CPU sensors additionally require
  `output_cpu_stats=1`. Broker loss terminates the sidecar so procd owns retry
  policy, while retained LWT marks the instance offline.

SQM integration:

- `luci-app-cake-autorate-rs` is intended to be the single LuCI UI for SQM setup
  plus autorate control.
- Installing the Full pair pulls `sqm-scripts`, `uclient-fetch`,
  `nftables-json` and `speedtest-go`. `sqm-scripts` provides the normal OpenWrt
  CAKE/IFB stack; `uclient-fetch` remains available for reflector discovery and
  the explicitly untrusted legacy transport diagnostic, not as a speed-test
  fallback. Full Auto-Tune transport validation uses the Rust probe.
- The LuCI package declares `PROVIDES:=luci-app-sqm` and `CONFLICTS:=luci-app-sqm`
  as the build-time replacement intent. Final OpenWrt 25.12 APK v3 metadata
  verification confirms that the generator emits the provide but omits a
  runtime conflict field. Remove the standalone `luci-app-sqm` before installing
  this replacement; do not rely on the live APK solver to reject both UIs.
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

## Runtime notes

### Configuration validation

The audit-remediation parser separates pure per-instance values from reflector
URL loading, global history reads and interface discovery. Non-finite numbers,
inverted rate/threshold tuples, invalid factors and unbounded detection windows
are rejected before those discovery steps. Full and Lite now validate the
parsed LuCI candidate before saving. Source/browser checks do not replace the
remaining installed-RPCd, real-UCI, lifecycle and device acceptance gates.

Detection windows retain their configured length, not allocator capacity.
Native windows are limited to4096 samples, with a1MiB logical sample budget for
combined controller/reflector history (not a measured RSS bound). Parallel
pingers use the existing Lite limit of64; the minimum ping interval is0.05s.
Core timer intervals fit the signed32-bit millisecond domain (about24.9days).
A disabled direction may retain a valid min/base/max tuple or use all zero;
mixed zero tuples are invalid. Existing meaningful defaults remain unchanged.

Full/Lite rate-triplet feedback uses native-exported bounds and defaults instead
of separate UI rate floors. Decimal and scientific notation are parsed strictly;
trailing junk and non-finite values are rejected. Other scalar and cross-field
constraints are checked by the same native candidate validator before saving,
including activity thresholds, positive timers, coefficient order and bounded
windows. A form accepting typed input is not itself permission to save or apply.

A stdin-only `--validate-config-candidate` entry point checks
submitted controller and managed-SQM intent and returns a versioned JSON result.
Its `validation_scope` is `controller-sqm`, not complete lifecycle/Apply
acceptance. It performs no UCI reads, reflector requests, temporary-file writes
or service actions. MQ admission reads local script and boot/tool/module-bound
proof; it never launches a capability probe as a side effect of validation.

The Full/Lite SQM selector offers CAKE and, after verification, multi-queue
CAKE. SQM receives `qdisc=cake` plus `use_mq=1`, not the legacy `cake-mq` name.
Legacy `cake-mq`/`cake_mq` intent and retained SQM `use_mq` remain visible;
an explicit single-queue choice writes `sqm_use_mq=0`. An unverified saved MQ
choice is shown with a warning, not silently disabled or offered as a new
supported choice. Native projection also rejects unsupported qdiscs and MQ.

"Verify multi-queue support" checks the selected script declaration and probes
kernel/tc using one private, down IFB with two queues. It generates no test
traffic, assigns no address, and does not alter existing interfaces or queues.
An owned receipt is written before creation; cleanup must finish before support
is published. Interrupted cleanup is recovered before another probe can start.
Proof does not survive a changed boot, tc or kernel module identity. Authorized
mutating startup/Apply preparation refreshes missing proof before applying the
SQM projection; read-only status, dry-run and candidate validation never do so.
Successful source tests are not proof of support on a particular router.

The native chunk-transfer bridge is also under source validation. It stages
at most four256KiB candidates in a private0700 directory with0600 files, using
1024-byte chunks, immutable request/length/SHA-256 identity and idempotent chunk
replay. Terminal validation/cancel removes the record. Admission expires after
120seconds; interrupted leftovers are retired on the next transfer request,
not by a new polling daemon. The namespace is excluded from diagnostic export.
This controlled staging does not grant LuCI general file-write authority.

### Stop with pending configuration

Ordinary Stop now captures committed `cake-autorate` and `sqm` through private,
random UCI package aliases. It retains that snapshot for planning and SQM stop
checks, preserving unrelated default/rpcd savedir contents. Changed committed
files, native Apply recovery, busy lifecycle ownership and unsafe runtime state
remain refusals. This is not permission to commit or revert pending settings.
Safe busy/stop diagnostics also reach stderr for LuCI, while rc.common's
fail-closed exit remains in place.

Start still uses its existing staged-change guards pending the separate R4
isolated-transaction work. Stop source/fixture checks do not replace installed
RPCd, running-service, package-upgrade and physical-device acceptance.

The source also has a journaled two-package file publisher and a
private controller-input consumer. The latter keeps controller argv unchanged:
an internal procd environment value identifies a content-bound generation.
Missing, corrupt or mismatched input never falls back to ordinary UCI. Input
files are private and excluded from diagnostic export; their producer, runtime
acceptance, retirement and scoped SQM Start still require lifecycle integration.
These components are not a completed Start cutover or a deployment claim.

### Bounded daemon logging

The audit-remediation source keeps one active `cake-autorate.<instance>.log`
and one plain-text `.old` file. Each is limited by `log_file_max_size_KB`
(2000 KiB by default), with a hard maximum of16384 KiB per file even when the
configured value is zero. Records are UTF-8-safe and bounded; oversized existing
logs retain only a bounded complete-record tail with a truncation marker.
The private zero-byte `.lock` file serializes writers; a bounded `.tmp` file may
exist during compaction. Neither is included in diagnostic downloads.

Rotation no longer runs gzip or logger in the controller loop. SYSLOG/ERROR
messages also reach procd stderr; repeated file-write failures are throttled.
If file logging cannot open safely, the controller reports that failure and
continues with system logging. MQTT drains unread data from its old descriptor
before switching to the replacement file, in bounded event-loop batches.

Old numeric timestamp archives belonging to the configured log basename are
removed in bounded startup/write batches. Unrelated names, symlinks, hardlinks
and foreign-owned files are preserved. Large pre-existing archive collections
may therefore require several batches; normal rotation never creates more.
Legacy `log_file_export_compress` values remain readable for compatibility but
do not enable runtime compression. LuCI diagnostic downloads remain plain text,
including safely decoded older gzip archives.

The LuCI Logging tab validates the built-in native MQTT publisher. No external
MQTT client package is needed. After setting `mqtt_enabled=1`, `mqtt_host`, and
the required summary logging options, restart `cake-autorate`; its Full-only
MQTT sidecar creates Home Assistant discovery sensors and publishes instance
state under the configured base topic.
