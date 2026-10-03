# Testing and observed results

This page separates reproducible validation from product claims. Internet
speed tests depend on the selected server, routing, client and background
traffic; the measurements below describe one test setup and are not a
universal benchmark. Older release evidence is kept in the
[testing archive](TESTING_ARCHIVE.md).

## RC27 r320/r128 release acceptance

This summary covers the RC27 r320/r128 release (daemon r320, Full LuCI r128,
Lite r320/r8, speedtest-go 1.7.10-r5), built after the audit remediation of
commit 074e221 in September 2026.

### Source gate

Every change passes one scripted source gate of 53 commands: Full (1,777) and
Lite (419) Rust tests, formatting, LuCI JavaScript unit tests, TypeScript
checks, a real-Chromium DOM text-safety test, shell syntax, the speedtest-go
backend patch tests and a hash of all sources before and after the run. A gate
is void if any source changes while it runs.

### Package matrix

Full, Full LuCI and Lite LuCI packages are built in the OpenWrt 25.12 SDK for
all 12 ABIs and verified for payload, metadata, dependencies and ELF
interpreter/ABI; the LuCI payloads are identical across all SDKs. The
speedtest-go backend (1.7.10-r5, CLI only) is built from the repository recipe
after its race-detector patch tests and verified for the same 12 ABIs. The
release set (38 APKs) is bound to its source commit by `release-manifest.json`,
`matrix-report.json` and `SHA256SUMS`, and the new history, the decoded
payloads and the public metadata pass a pinned secret scan.

### VM acceptance (x86_64, OpenWrt 25.12)

- Full Raw Auto-Tune with an explicit Unlimited policy reached Review with
  three independent qualified servers; configuration and CAKE topology were
  restored exactly. A capped shaped-only run stayed within its budget.
- Speed Test and Auto-Tune over an explicit policy route (source, table, mark
  and route DNS): Speed Test completed with owned-traffic accounting within
  0.4% of interface counters; a Full Raw run reached Review (97.9%); the
  scheduler launched and completed an explicit-route run by itself.
- A dead split-default "VPN" route (0/1 and 128/1 via a black-hole interface):
  main-table mode refused probes and tests before any traffic; the explicit
  route kept working and sent no bytes into the VPN interface.
- Full → Lite → Full replacement with the documented `apk` procedure restored
  services, UCI, qdiscs and the apk world.
- Privacy: no unsolicited TCP/UDP by default; external transport probing only
  after opt-in, verified across restarts.
- Browser: the installed LuCI wizard ran a capped Auto-Tune to Review with all
  UCI writes blocked. A scripted read-only visual audit covers Status, Graphs,
  Settings, Traffic priorities, the instance editor and the Auto-Tune wizard at
  desktop (1440 px) and phone (390 px) width in light and dark Bootstrap, on
  the VM and on a physical router, with no page-level horizontal overflow.

### Physical routers

- x86_64 router, PPPoE uplink (~900 Mbit/s) and a secondary uplink: Full Raw
  Unlimited Auto-Tune reached Review on both; owned-traffic accounting was 97–98%
  of interface counters; a capped browser-started run reached Review without
  UCI writes; a real Apply of the recommended option updated UCI and CAKE
  rates and left network, firewall and mwan3 unchanged.
- ARM64 (aarch64_generic) LTE router with a 100 MB root filesystem: the
  CLI-only speedtest-go package fits where the previous 25 MB package failed
  with ENOSPC; Full Raw Auto-Tune reached Review (accounting 99.3%).

### Auto-Tune robustness (server failures, raw controls, early stops, Gaming)

- Server failure retries: on the VM, TCP resets for the backend's user for
  5 s during a search measurement produced two logged retries
  (`speedtest-backend-failed`); the third attempt succeeded and the run
  reached Review. With 15 s of resets the retries were exhausted and the
  measurement was reported as unmeasurable instead of a terminal failure.
  Unit tests cover the retry limit (0–10), the request schema family and
  evidence replay up to eleven charged attempts. With an explicit limit the
  same fault logged no retry for 0 and exactly one for 1 before the
  measurement was marked unavailable; configuration and CAKE topology were
  restored both times.
- Launch from LuCI with a retry count: the daemon first refused every such
  start without a route mark (main routing) with "operation schema does not
  match its target lifecycle". After the fix, a wizard start on the VM
  (main routing, 1 GB, 1 retry) and on the LTE router (Variable link, Full
  raw, Unlimited, 2 retries) reached Review; the LTE Review offered
  recommended, lowest-latency, highest-throughput and download-without-shaping
  options. A shared fixture of the exact LuCI launch argv (new and existing
  instance, main, mwan3 and explicit routes, capped and Unlimited, with and
  without retries) is decoded by the daemon test through the same Start path.
- Server switch (`tools/audit-vm-server-switch.py --mode selected-server`):
  after the comparison selected server 17372, every backend connection to its
  address was reset for the rest of the run. The first raw download control
  failed three times and moved download to 29062; the raw upload control then
  failed three times and moved upload to 29062. Both switches restarted the
  measurements from the raw controls, and the Gaming run reached a complete
  Review with CAKE options (`recommended`, both shaped; `bypass_download`,
  upload shaped). The journal held two `server_switch` records; topology and
  configuration were restored and the fault table was removed.
- Incomplete upload search (`--mode upload-search`): backend connections were
  reset only while the job searched its upload rate. Upload failed on 17372
  and moved to 37193, the measurements restarted, and upload failed again
  with no qualified source left. Review (public schema 7) offered
  `partial_quality_first` (preselected for Gaming) and
  `partial_throughput_first`, both shaped with the exact rates that ran during
  the download search measurements, acknowledging
  `upload-search-incomplete` and `upload-shaped-load-unmeasured`, plus
  SQM disabled as a non-preferred option.
- x86_64 PPPoE router with mwan3, LuCI wizard, Gaming, Full raw,
  Unlimited, 2 retries: the run reached a complete Review with
  `recommended` (both shaped, 845.9/901.2 Mbit/s, class A+, Auto-Apply checks
  passed) and `bypass_download`. No switch was needed on server 12456, no
  route event occurred during the run, and nothing was applied.
- Raw control below the server comparison: with the raw upload control
  throttled to about 120 Mbit/s against a 317 Mbit/s comparison, the Full Raw
  run continued to Review; every option carried
  `upload-raw-below-server-comparison`, and the read-only Apply check
  returned `confirmation_ready` with the same acknowledgement list.
- Early stop: a DOM test checks that a stopped test states that the current
  settings are unchanged, explains the reason and offers to keep them. A
  start the service refuses is shown as "did not start", with nothing
  measured and no test traffic used.
- Re-calibration of an already tuned PPPoE instance on the x86_64 router
  reached Review with two options instead of stopping: a shaped option at
  about 904/913 Mbit/s that needed no acknowledgements, and a preselected
  no-shaping option. Accounting was 98% of interface counters.
- Gaming on the LTE router: the first candidate's samples did not repeat
  within 5%, so the search stepped down (to 219, 179, 159, 149 and
  144 Mbit/s download) instead of falling back to the raw result. Review
  offered a recommended lowest-delay option and a throughput-first option
  with a higher upload rate, each listing the capacity and latency
  acknowledgements it needs. Accounting was 99.6% of interface counters, and
  the CAKE topology was restored.

### Known limits

- IPv6-only uplinks are not supported.
- Explicit policy routes were tested with a simulated VPN interface, not a
  real WireGuard or OpenVPN tunnel.
- Lite was verified on the VM; physical routers ran Full.

## Changes after RC27 r320/r128 (unreleased)

These results cover the commits made after the r320/r128 release: the server
switch window, the cap on measurement-basis restarts, strict traffic-rule
validation, the finite-value check in the rating code, the plain-MQTT notice and
the topology repeat fix described below.

### Source gate and packages

- Source gate, 53 commands: Full (1,758) and Lite (391) Rust tests, formatting,
  LuCI JavaScript tests, TypeScript checks, real-UCI tests and a hash of 230
  sources before and after the run, which did not change.
- During development the package matrix covers x86_64 and the ARM test-router
  architectures (aarch64_cortex-a76 and aarch64_generic): all three were built
  from that exact source in the OpenWrt 25.12 SDK and verified for payload,
  metadata, dependencies and ELF interpreter/ABI. The full 12-ABI matrix is
  built for a release.

### Real nftables

- Two ignored unit tests apply the traffic classifier to a real nftables 1.0.9
  in a throw-away user and network namespace (procedure in
  [DEVELOPMENT.md](DEVELOPMENT.md)). They showed that nftables merges repeated
  and overlapping ports itself, that a leading `+` is a syntax error and that a
  prefix with a leading zero is read as octal: r320 installed `10.0.0.0/024` as
  `/20`, the current daemon renders and installs `/24`.
- On the test VM (nftables 1.1.6) the same edge cases pass `nft -c`, and the
  installed daemon renders a custom ruleset with repeated and overlapping ports
  and a leading-zero prefix that `nft -c` accepts, with the prefix as `/24` and
  the ports merged to `80-90,443,1000-2500`.

### VM acceptance (x86_64, OpenWrt 25.12)

- The candidate was installed with `apk`; the installed and the running daemon
  binaries matched the verified build and the configuration files were
  unchanged.
- Server switching, Gaming, Full Raw Auto-Tune with an explicit Unlimited
  policy, speed-test connections reset by a private nft table:
  - `upload-search` (every backend connection reset while the upload rate is
    searched): reached Review after two server switches with two measured CAKE
    options next to SQM disabled; configuration and CAKE topology restored.
  - `selected-server` (the selected server unreachable for the whole run):
    each direction moved to another qualified server and the run reached
    Review. One run found that every pair option missed the 50% retention floor
    and offered SQM disabled only; another confirmed a pair that was not the
    recommended one and offered a single `throughput_first` CAKE option
    (614/281 Mbit/s) after the topology comparison.
- The first candidate failed the `selected-server` scenario at the topology
  comparison. Review is built on the first pair option that keeps the 50%
  retained-capacity floor, which is not always the point the directional
  searches recommended (183/93 Mbit/s kept about a quarter of the download
  capacity; the confirmed pair was 748/282 Mbit/s). The raw repeats were shaped
  at the confirmed pair but the journal replay demanded the recommended rate, so
  the run failed after its long measurement; r320 has the same defect. The fix
  binds each repeat to the confirmed pair. It is covered by a unit test, by an
  offline replay of the real journal of the failed run and by the live run
  above.

### Known limits

- A live run cannot force the situation in which the recommended pair misses the
  retention floor; it depends on the link, so the unit test and the replay of a
  real journal carry that path.
- Lite and the physical routers were not run for these changes beyond the source
  gate.

## Changes after RC27 r320/r128: operation deadlines and overflow checks (unreleased)

These results cover the commits that move the time limits of Speed Test, Rating
and Auto-Tune, and the freshness check of the runtime snapshot, from the wall
clock to the boot clock, and the decision to keep integer overflow checks in the
release build. They were made on top of the commits in the previous section.

### Source gate and packages

- Source gate, 53 commands: Full (1,779) and Lite (391) Rust tests, formatting,
  LuCI JavaScript tests, TypeScript checks, real-UCI tests and a hash of 231
  sources before and after the run, which did not change.
- All twelve matrix architectures were built from that exact source in the
  OpenWrt 25.12 SDK and verified for payload, metadata, dependencies and ELF
  interpreter/ABI. The SDK build log of every architecture shows
  `-C overflow-checks=on` on both daemon compilations (Full and Lite).

### Clock steps

- Unit tests script the wall clock and the boot clock separately
  (`identity::test_clock`). They step the wall clock forward by hours and back
  by minutes, and back past the creation time of a job. A job's deadline neither
  moves nor expires early; a queued job neither expires nor is extended; a
  runtime snapshot stamped ahead of the reader's clock, or too old, makes the
  job wait instead of failing; the capture request of a Rating run keeps its
  deadline when it is published again; the scheduler does not evaluate a
  schedule while the wall clock is before 2026.
- VM acceptance (x86_64, OpenWrt 25.12). The clock was changed on the test VM
  only: NTP stopped for the run, the clock set with `date -s`, set back from the
  host clock afterwards and NTP restarted. Most runs used the build of the
  previous source gate; its x86_64 daemon equals the final one byte for byte
  apart from the build path embedded in it, and the final build was run again
  with a Speed Test and an Auto-Tune (below).
  - Speed Test (capped, current topology) with four steps (+2 h, -5 h, +3 h,
    +15 min) while it ran: completed in 25 s. Speed Tests on the shaped and on
    the unshaped topology with three and four steps completed the same way.
  - Full Raw Auto-Tune (explicit Unlimited policy) with four steps (+2 h, -5 h,
    +3 h, +10 min): reached Review in 13 minutes with the topology restored.
    Two earlier runs with two and four steps reached Review in 9 minutes.
  - Control: the build before these changes failed an Automatic Rating at the
    first +2 h step with `operation-deadline-expired-during-capture-handshake`.
  - One Auto-Tune run failed during server qualification with
    `speedtest-rate-invalid` after a +2 h step. Direct runs of the backend with
    and without steps, two further Auto-Tune runs and Speed Tests on the
    unshaped topology with steps all passed; the failure was not reproduced and
    its cause is not established.

### Overflow checks

The OpenWrt SDK already builds every Rust package with overflow checks, so the
checks were measured against a build of the same source with that default
switched off in a private SDK copy (see [DEVELOPMENT.md](DEVELOPMENT.md)). The
manifest now states the setting; the packages are unchanged.

- Size, all twelve architectures: the Full binary grows by 0.6% to 1.1%, the
  Lite binary by 0% to 2.3% (one 64 KiB segment step on MIPS), the packages by
  0.3% to 0.7%.
- CPU on the test VM, each build measured twice for 3 minutes idle and 4 minutes
  of repeated capped Speed Tests: the controller used 0.39% (checks on) and
  0.36% (off) of one core idle, 1.22% and 1.23% under load, within the spread of
  repeated runs. Host micro-benchmarks of the Auto-Tune search and the
  controller's text parsers: 1.4% slower with the checks.
- Review and stress: every subtraction and multiplication that clippy's
  `arithmetic_side_effects` lint flags was read against its guard. The
  Auto-Tune search, the routed DNS answer parser and the controller's text
  parsers ran with the checks on against about 3.4 million random and mutated
  inputs without a panic.
- Soak on the VM with the shipped build (checks on), 55 minutes: 39 capped Speed
  Tests, 6 Automatic Ratings and 3 Full Raw Auto-Tune runs (explicit Unlimited
  policy) that reached Review with the topology and the fixed settings
  restored. No process restarted, and the system log, the controller log, the
  job logs and `dmesg` held no panic or overflow message.

### Known limits

- A Rating run collects its quality grade in wall-clock windows. A step of the
  wall clock during the capture makes the controller drop the samples collected
  so far, so that run does not finish before its deadline; repeat it. Speed Test
  and Auto-Tune are not affected.
- Scheduled calibration follows the calendar and keeps using the wall clock.
  It does not start while the clock is before 2026, that is before the router
  has synchronised.
- Lite and the physical routers were not run for these changes beyond the source
  gate.

## Changes after RC27 r320/r128: MQTT over TLS (unreleased)

These results cover the commits that add TLS to the native MQTT publisher
(`mqtt_tls`, `mqtt_tls_ca_file`, `mqtt_tls_verify`), the LuCI settings for it,
and the retry at start-up that the TLS work showed to be necessary. They were
made on top of the commits in the previous sections.

### Source gate and packages

- Source gate, 54 commands: Full (1,810) and Lite (391) Rust tests, formatting,
  LuCI JavaScript tests, TypeScript checks, real-UCI tests and a hash of 234
  sources before and after the run, which did not change.
- All twelve matrix architectures were built from that exact source in the
  OpenWrt 25.12 SDK and verified for payload, metadata, dependencies and ELF
  interpreter/ABI.
- Size, compared with the build before the TLS work (the Full daemon already
  contained rustls for the calibration client, so only the MQTT glue and the
  trust-anchor handling are new): the Full binary file grows by 24 to 32 KiB
  on x86_64 and ARM (0.4%) and by one 64 KiB step on MIPS (0.7%). On aarch64
  the file size does not change because its segments are aligned to 64 KiB;
  the read-only segment grows by 22 KB. The Full package grows by 12.9 to
  16.6 KB on every architecture (0.39% to 0.46%). The Lite daemon has no MQTT
  and is unchanged on every architecture. `rcgen`, which makes the test
  certificates, is a development dependency: it is in `Cargo.lock` but is not
  compiled into any package.

### Tests without a router

- 29 Rust tests run the publisher against a TLS server inside the test
  process (rustls server, certificates made by `rcgen`):
  - the whole MQTT conversation over TLS with a private CA, addressed by host
    name and by IP address; a wrong CA; the built-in roots; an expired, a not
    yet valid and a wrong-name certificate; verification switched off; a CA
    file that is missing; a TLS client against a plain broker (the error says
    that the port may not be TLS); a silent broker (the handshake gives up
    after 5 s); a single self-signed certificate, with and without the CA flag;
  - several MQTT packets in one TLS record: all of them are read in one
    wake-up, none is left waiting in rustls's decrypted buffer, where `poll` on
    the socket cannot see it;
  - the status handshake: it completes the TLS handshake and sends no MQTT
    packet, so no user name or password leaves the router;
  - the CA file rules: absolute path of at most 1024 bytes, regular file, not a
    symbolic link, owned by root, not writable by group or others, 1 byte to
    1 MiB, at most 512 certificates (a bundle of 200 certificates is accepted;
    the real 121-certificate bundle of the build host was also read once);
  - the start-up retry: pauses of 5, 10, 20, 40 and 60 s, a stop request that
    ends the wait at once, and the policy for repeating an unchanged error.
- Upgrade invariant, checked against bytes recorded before the change:
  - a configuration without `mqtt_tls` is plain MQTT on port 1883, a plain
    broker's plan has exactly the bytes (version 1) that the release before TLS
    wrote, and a plan file of that release on disk decodes and is unchanged
    when the same configuration is planned again; a TLS plan uses version 2,
    and every field that does not apply is dropped, so an unused TLS setting
    cannot restart a publisher.
- LuCI (JavaScript tests and a real browser): `mqtt_tls` is on by default for
  new settings; a saved publisher without the option shows it off, with the
  plain-MQTT notice (the A5 warning appears only then); the CA file and
  Verify certificate fields, the unverified and port-1883 notices, the port
  placeholder and the Check MQTT result lines.

### VM acceptance (x86_64, OpenWrt 25.12)

The brokers were a real Mosquitto 2.0.11 (OpenSSL 3.0.2) on the Linux test VM:
one plain listener, four TLS listeners and a TLS 1.2-only proxy. The MQTT user
and password were made up for the test. The run starts with the daemon of the
Goal D candidate (no TLS), installs the candidate, and puts everything back at
the end: the router configuration is byte-identical, no `mqtt_*` option is
left, the clock is within a second of the host and NTP runs; Mosquitto is
removed from the Linux VM. The run was made on the build of the previous
source gate and again, after the last source change (the CA file limits and two
corrected texts), on the final build (daemon `0e3efcca…`): 27 scenarios and 207
checks, none failed in either run.

- Upgrade: the plain publisher of the old daemon keeps its plan bytes after
  the candidate is installed (same SHA-256), connects to port 1883 as before,
  `--mqtt-status` reports `tls: false` and `not_checked` (the broker is not
  contacted), and a `reload` with no change leaves the publisher alone. The
  real-browser LuCI page for that legacy configuration shows TLS off.
- TLS: with a CA file the publisher connects (TLS 1.3, and TLS 1.2 against the
  1.2-only proxy) and the broker receives discovery, availability and state on
  port 8883; without a CA file a private broker is refused (`UnknownIssuer`);
  a wrong CA, a certificate for another name, a CA certificate used as the
  broker's own (`CaUsedAsEndEntity`, with the hint to issue a CA:FALSE
  certificate), a TLS client on a plain port (hint that the port is not TLS),
  a missing CA file and a group-writable CA file are all refused with the
  reason in the system log; the publisher stays the same process and keeps
  retrying. Verification off connects and the log says that the certificate
  was not verified. Every case was also checked with `--mqtt-status`.
- Reload (what Save & Apply runs): switching to TLS, a different CA file path,
  a changed verify flag and a changed port restart the publisher, and the
  broker sees the new connection on the expected port; a reload with an
  unchanged configuration restarts neither the publisher nor the controller,
  for plain MQTT and for TLS; a publisher that cannot reach its broker (unknown
  CA, refused port) no longer makes the reload fail or leaves the service
  stopped (see below).
- Broker: a lost connection ends the process and procd starts a new one; a
  broker that is away for more than a minute does not restart the publisher,
  which connects by itself when the broker is back; the last will (`offline`,
  retained) is published after `kill -9`, a clean stop publishes `offline`
  itself, and an idle publisher stays connected over TLS across the broker's
  keepalive limit (100 s without any restart).
- Clock (changed on the VM only, restored from the host afterwards): with the
  router clock behind the certificate the log says `not valid yet`, with the
  clock ahead `certificate expired`; the publisher keeps retrying as the same
  process and connects by itself when the clock is right.

### The reload hazard that the TLS work exposed

With the daemon of the current release, `/etc/init.d/cake-autorate reload` (the
command behind Save & Apply) fails with `MQTT startup deadline expired` and
`native reload failed; inspect retained recovery state` when it restarts a
publisher that cannot connect, for example a plain broker that refuses the
connection, and the controller is left stopped. A later `start`, `reload` or a
corrected configuration is refused with `uci-transaction-source-receipt-changed`
until the router is rebooted. TLS makes the trigger much easier (an unknown CA,
an expired certificate, a clock that is not set yet). The lifecycle waits for
the publisher's start-up receipt and then needs the same process to be alive in
procd, and a publisher that could not connect exited within milliseconds. The
publisher now retries by itself when a start cannot reach the broker (5 s
growing to 60 s) and only an established connection that is lost ends the
process. It was reproduced on the test VM with the unchanged daemon of the
current release (MQTT enabled, a broker address that nothing listens on,
`uci commit`, `reload`), and the same failure followed in the first TLS
candidate with a CA file that did not match the broker's certificate. With the
final daemon both reloads succeed, the controller and the publisher stay up,
and the publisher keeps retrying.

### Known limits

- The dates of the broker's certificate are judged by the router's clock. A
  router that has not synchronised its time yet retries until it has.
- The CA file is read when the publisher starts. After replacing it, restart
  the service.
- Client certificates, MQTT over WebSocket and brokers limited to TLS 1.1 or to
  non-ECDHE cipher suites are not supported.
- `--mqtt-status` tells the result of a handshake made when it is called; the
  last error of the running publisher is only in the system log.
- After a native reload that failed for some other reason, the lifecycle's
  retained recovery state still asks for the exact sources of the failed
  transaction, and only a reboot clears it. That was already so and is not
  changed here.
- Lite has no MQTT. The physical routers were not run for these changes.
