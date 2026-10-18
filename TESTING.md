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

## Changes after RC27 r320/r128: unsafe code (unreleased)

These results cover the commits that put the daemon's direct libc calls behind
safe wrappers (`src/sys.rs`), write a SAFETY comment for every `unsafe` block
that remains, and make the source gate check both. The behaviour of the daemon
is meant to be unchanged; the VM runs below compare the build before and the
build after. They were made on top of the commits in the previous sections.

### Source gate and packages

- Source gate, 57 commands: Full (1,827) and Lite (407) Rust tests, formatting,
  `cargo clippy --all-targets` with `clippy::undocumented_unsafe_blocks` denied
  for the Full and the Lite build (daemon and tests), a check that no local
  `extern "C"` block is left, LuCI JavaScript tests, TypeScript checks, real-UCI
  tests and a hash of 235 sources before and after the run, which did not
  change. The clippy command was tried on a block without a comment: it exits
  with an error ("unsafe block missing a safety comment"); so was the extern
  check on a local declaration.
- All twelve matrix architectures were built from that exact source in the
  OpenWrt 25.12 SDK and verified for payload, metadata, dependencies and ELF
  interpreter/ABI. The SDK build log of every architecture shows
  `-C overflow-checks=on` on both daemon compilations.
- Size, compared with the build before this work: the Full binary file grows by
  0 to 4 KiB (0.06% at most, one 4 KiB page step on x86_64 and ARM, none on
  aarch64 and MIPS), the Full package by 1.4 to 6.2 KB (0.04% to 0.18%), the
  Lite binary by 0 to 4 KiB.

### What changed

- `clippy::undocumented_unsafe_blocks` reports 0 blocks in the daemon and in its
  tests, in both builds. It reported 211 (Full) and 102 (Lite) in the daemon and
  56 more in the tests. About 60 `unsafe` blocks remain, half of them in
  `src/sys.rs`; each has its own comment saying why it is sound.
- The calls with a plain signature are safe functions in `src/sys.rs`: user,
  group and process ids, `flock`, `pidfd_open`, `poll`, `eventfd`, `timerfd`,
  `inotify`, `read` and `write` of counters and buffers, `kill` and `kill` of a
  process group, `setsid` before exec, `sysconf`, `renameat2`, `socket`,
  descriptor duplication, `set_nonblocking`, signal sets, `signalfd` and zeroed C
  structures. The wrappers retry nothing and keep errors as the callers saw
  them; `kill` refuses a pid of zero or below and a process group of 1 or
  below. 16 unit tests cover them (a busy lock is `WouldBlock`, a pidfd turns
  readable on exit, counters and timers, inotify, hung-up pipes, descriptors are
  close-on-exec, a blocked signal arrives through its descriptor, `renameat2`
  exchanges and refuses to replace, a `pre_exec` error reaches the parent as the
  same error number), and one more tests the error of the speed-test backend.
- The eight local `extern "C"` blocks are gone: what they declared comes from
  the libc crate. Two things differ from before on purpose. The coordinator's
  peer-credential check used `SOL_SOCKET = 1` and `SO_PEERCRED = 17`, which are
  right on x86, ARM and AArch64 but not on MIPS (`0xffff` and 18), where
  `getsockopt` would have failed; it uses the libc constants and `libc::ucred`
  now. The `pre_exec` closure of the speed-test backend built an error message
  after fork, which allocates; it returns an OS error and the parent says "the
  speedtest worker disappeared before the backend started".

### VM acceptance (x86_64, OpenWrt 25.12)

"Before" is the build of the previous goal (daemon `0e3efcca…`), "after" the
build of this one (`99062905…`), both from the SDK matrix. The host was quiet
unless the text says otherwise.

- Service lifecycle (33 checks): stop, start, restart and a reload with an
  unchanged configuration; SIGTERM and SIGINT to the controller (it exits and
  procd starts a new one); restart and SIGTERM of the calibration daemon;
  `--cpu-profile 5` and `--mqtt-status`; no panic, overflow or fatal signal in
  the system log or `dmesg`. The PASS lines of the two builds are identical.
- Speed Test (capped, current topology): completed in 24.9 s (25 s before).
- Full Raw Auto-Tune (explicit Unlimited policy): reached Review in 660 s (9 to
  13 minutes before), with the topology restored.
- MQTT publisher and TLS against a real Mosquitto (the acceptance of the TLS
  goal: upgrade, the TLS cases, reload, broker loss and return, a long outage,
  the last will, clean stop, keepalive; the clock block was left out, it does
  not touch this code): 193 checks pass for the build before, and for the build
  after on a quiet host and with the host loaded by 96 busy loops. A first full
  run of the build after (207 checks) had three failures while the host was
  busy with two SDK builds and other jobs: two reloads and one restart ended
  with `native reload failed` or `native service stop transaction failed`. See
  the next section; the same scenarios passed on every rerun.

### A reload or restart can fail while the host is busy (not new)

The failures above have a cause that is older than this work. The controller
starts short-lived helper processes (`ip`, `tc`, `uci`), and between `fork` and
`exec` such a child has the command line of the controller. The service
lifecycle looks for "exactly one controller per instance" in `/proc` and
refuses to continue when it sees two: `multiple exact controller processes are
running for primary`. On an idle router that window is microseconds wide; when
the CPU is contended it stretches, and a reload or restart that scans at that
instant fails and has to be repeated (the next attempt succeeded every time;
the retained recovery state of the failed attempt is the one described in the
section on MQTT over TLS). A sampler on the VM (about 170 samples a second over
`/proc/*/cmdline`) recorded a child of the controller with the controller's
command line, or already `ip -4 rule show` / `tc qdisc show` / a zombie, with
the same parent, for both builds. Sixty reloads in a row with the MQTT publisher
enabled, the MQTT host not listening:

| host | before | after |
|---|---:|---:|
| quiet, publisher off, 30 reloads | 0 failed | 0 failed |
| 96 busy loops on 64 CPUs, publisher on, 60 reloads | 4 failed | 2 and 4 failed (two runs) |

In steady state, four alternating five-minute samples under the same load saw a
second process with the controller's command line 9 times for the build before
(8 and 1) and 8 times for the build after (3 and 5).

The failures were `multiple exact controller processes are running` in all but
one case (`controller-batch-readiness-changed`, the readiness check flipping
between two looks). The lifecycle is not changed here; a scan that ignores a
process whose parent is the controller would remove the first cause.

### Known limits

- A reload or restart on a router whose CPU is saturated can fail as described
  above; repeating it works.
- Nothing was run on MIPS hardware; the MIPS builds compile the corrected
  constants.
- Lite and the physical routers were not run for these changes beyond the source
  gate and the package checks.

## Changes after RC27 r320/r128: speedtest-go r6 (unreleased)

These results cover the commit that gives the `speedtest-go` backend the TLS
defaults of current Go and builds it as a position-independent executable
(revision 6), and that raises the backend floor of the Full daemon package to
`speedtest-go >= 1.7.10-r6`. No daemon source changed.

### Source gate and packages

- Source gate, 57 commands: Full (1,827) and Lite (407) Rust tests, formatting,
  clippy with undocumented unsafe blocks denied, LuCI JavaScript tests,
  TypeScript checks and real-UCI tests, and a hash of 236 sources (the new patch
  is one of them) before and after the run, which did not change.
- Backend: all twelve ABIs were built from the repository recipe in a clean
  OpenWrt 25.12 SDK copy each (`tools/release-backend-matrix.py`). Every build
  runs the package tests offline with the race detector (`go test -race -run
  '^TestCake' -count=5`, which includes the six new TLS tests), and every APK is
  verified for payload, metadata and ELF interpreter and ABI. For revision 6 the
  verification also reads the Go build information of the shipped binary: the
  seven settings must have their modern values (`tls3des=0`, `tlsrsakex=0`,
  `tlssha1=0`, `tlsunsafeekm=0`, `rsa1024min=1`, `x509negativeserial=0`,
  `tls10server=0`) and the ELF type must be `DYN` where Go can build a PIE and
  `EXEC` where it cannot. All twelve passed. The SDK build logs of `mips_24kc`
  and `mipsel_24kc` show the warning `PIE buildmode is not supported for
  linux/mips`; the other ten built with `-buildmode=pie`.
- Daemon: the Full and Lite packages for x86_64 and aarch64_cortex-a76 were
  built from the same gate. The Full metadata depends on
  `speedtest-go>=1.7.10-r6`, Lite has no backend dependency, and both rustc
  invocations of every build use `-C overflow-checks=on`.

| ABI | ELF type | binary r5 -> r6 | APK r5 -> r6 |
|---|---|---:|---:|
| x86_64 | DYN (PIE) | +20 KB (+0.2 %) | +6 KB (+0.2 %) |
| aarch64 (4 ABIs) | DYN (PIE) | +12 KB (+0.1 %) | +6..7 KB (+0.2 %) |
| arm (5 ABIs) | DYN (PIE) | +0.53 MB (+6.3 %) | +165 KB (+4.9 %) |
| mips_24kc, mipsel_24kc | EXEC | unchanged | unchanged |

The binaries are 8.1 to 9.6 MB, the APKs 3.2 to 3.6 MB. On 32-bit ARM the growth
comes from the relocations a PIE needs (`.rel.dyn` of `arm_cortex-a9` goes from
8 bytes to 283 KB) and from larger position-independent code (`.text` grows by
235 KiB).

### What changed

- Patch 130 adds `cake_godebug.go` to the main package with one `//go:debug`
  line for each of the seven settings, which the module's `go 1.19` line would
  otherwise leave at their old values. The go command checks the directives when
  the program is built, so a later toolchain that no longer knows a setting
  breaks the build instead of ignoring the line. All other defaults of `go 1.19`
  are unchanged on purpose, among them `httplaxcontentlength=1` and the two
  settings that keep the hybrid post-quantum key exchange off (`tlsmlkem=0`,
  `tlssecpmlkem=0`).
- `cake_godebug_test.go` runs real handshakes over an in-memory pipe: a server
  with only an RSA key exchange suite and a server with only a 3DES suite must be
  refused, a TLS 1.0 client must be refused by a default server, TLS 1.2 and 1.3
  must still work, a 512-bit RSA key must be refused, and a certificate whose
  serial number is made negative must not parse. With the directive file removed,
  five of the six tests fail (the sixth, TLS 1.2 and 1.3 still working, is the
  control).
- `PKG_ASLR_PIE_REGULAR:=1` in the recipe; `PKG_RELEASE:=6`; the Full package
  requires `speedtest-go (>=1.7.10-r6)`; the package test, the backend README and
  the main README follow.

### VM acceptance (x86_64, OpenWrt 25.12)

The VM runs the daemon of the previous goal (`99062905…`) throughout; only the
backend changed. Backends were installed with `apk add --allow-untrusted` of the
verified APK (`tools/goal-b-vm-backend.py`); every install was a single-package
transaction, the installed binary was compared with the verified payload, and the
hashes of the daemon and of the four configuration files and the boot ID were
the same before and after.

- Dependency floor, `apk add --simulate` with the Full daemon APK from the gate
  above: with r4 installed the daemon alone is refused (`speedtest-go-1.7.10-r4
  breaks cake-autorate-rs-1.0_rc27-r320[speedtest-go>=1.7.10-r6]`); with r5
  installed the daemon alone and the daemon together with the r5 APK are both
  refused; with r5 installed the daemon together with the r6 APK is accepted and
  touches exactly the two packages; with r6 installed the daemon alone is
  accepted. None of the simulations changed the installed state.
- Server list: `speedtest-go --list` three times with r5 and three times with
  r6 returned the same 20 servers in 0.9 to 1.1 s without an error. The list is
  fetched from speedtest.net over HTTPS, so this is the TLS client of the
  installed binary.
- Servers of the list, probed from the build host with a small Go program built
  by the same Go 1.26.7 (so the same TLS code, run once with the defaults of
  current Go and once with the old defaults restored through `GODEBUG`:
  `tls3des=1,tlsrsakex=1,tlssha1=1,tlsunsafeekm=1,rsa1024min=0,
  x509negativeserial=1,tls10server=1`). The list names plain HTTP URLs on port
  8080, but 11 of the 20 servers answer with a redirect to HTTPS, and all 11
  negotiate TLS 1.3 (`TLS_AES_128_GCM_SHA256`), as do `www.speedtest.net` and
  `speedtest-config.php`. Two runs of each kind gave the same status for every
  server (a first, unsaved run saw one server reset the connection over plain
  HTTP, before any TLS). So no server of the list needs a TLS feature that r6
  turned off.
- Speed Test (capped at 150 MB, current topology, through the daemon, no clock
  steps), Tallinn #17372 except where noted. Kilobits per second, download /
  upload:

| backend | runs |
|---|---|
| r4 (before this work, same day) | 10,525 / 20,189 |
| r5 | 10,496 / 19,889; 9,653 / 20,212; 6,235 / 12,660 (slow minute, 42.9 s) |
| r6 | 9,729 / 20,234; 9,792 / 20,010; 9,758 / 20,047; 9,773 / 19,848 (server #37193) |

  All completed in 23 to 25 s (the slow r5 run took 43 s), and the job logs of
  the first r5 run and the first r6 run are empty. Server #37193 is one of the
  11 that redirect to HTTPS. The first r5 run was 7 % faster than the next r6
  runs, so r5 was installed again and measured: it gave 9,653 / 20,212, the same
  as r6, so the difference came from the path and not from the backend. The link
  varies by that much from one minute to the next.
- r6 is installed on the VM now; the service and NTP are running.

### Known limits

- `mips_24kc` and `mipsel_24kc` stay non-PIE: Go does not support `-buildmode=pie`
  there.
- The new tests drive five of the seven settings. `tlssha1` and `tlsunsafeekm`
  have no behavioural test; they are covered by the build-information check of
  every shipped binary only. The comment in `cake_godebug.go` and the backend
  README say that the tests fail when a weak default comes back, which is true
  for those five.
- Only the x86_64 binary was run (on the VM). The ARM, AArch64 and MIPS binaries
  were built and inspected, not executed.
- A server that redirects to a host which offers nothing but RSA key exchange,
  3DES or SHA-1 signatures would now fail where r5 succeeded. None of the 20
  servers of the list the VM sees do; other locations and later lists were not
  checked.

## Changes after RC27 r320/r128: MQTT TLS review fixes (unreleased)

These results cover three fixes found in a review of the MQTT over TLS work: a
slow broker could stretch the TLS handshake past its limit, a TLS record without
MQTT data could stop the publisher, and the settings page could turn a disabled
old publisher to TLS. Nothing else changed.

### Source gate and packages

- Source gate 186, 57 commands: Full (1,830, three more than before) and Lite
  (407) Rust tests, formatting, clippy, LuCI JavaScript and TypeScript checks,
  real-UCI tests, and a hash of the sources before and after the run, which did
  not change.
- Daemon and LuCI packages for x86_64 and aarch64_cortex-a76 were built from the
  same gate and verified (payload, metadata, ELF, overflow checks on).

### What changed, and the test that fails without it

- The handshake runs one write or read step at a time, each bounded by what is
  left of the 10 s limit, instead of `complete_io`, which keeps reading while the
  handshake needs more. `a_broker_that_dribbles_bytes_cannot_hold_the_tls_handshake_open`
  sends a record header and then a byte every 200 ms; with the old code it never
  returned, now it fails with "timed out" after the limit (1 s in the test).
- A readable socket is read without blocking before the MQTT read; a record
  without application data sends the publisher back to `poll`. A closed
  connection is still an error.
  `a_tls_record_without_mqtt_data_does_not_stop_the_publisher` has the broker
  send a TLS 1.3 KeyUpdate 1.2 s before the next packet: the old code waited
  1.19 s inside the read, the new code returns at once, still reads the
  PINGRESP behind it and still reports a DISCONNECT. A second test checks that a
  broker that just closes the socket ends the wakeup with an error.
- LuCI: the form shows plain MQTT for a section that holds any MQTT setting but
  no `mqtt_tls`, enabled or not. `tests/mqtt-tls.test.js` has the cases (a
  disabled publisher with or without a host, a host without the flag, empty
  values, a disabled publisher that chose TLS); the first fails without the fix.

### VM acceptance (x86_64, OpenWrt 25.12)

The MQTT harness (`goal-c-vm-mqtt.py`, a real Mosquitto on the Linux test VM,
clock block left out) ran on the candidate: 194 checks passed and two failed,
neither a defect of the product:

- `old status has no TLS fields` assumes the installed "old" daemon predates TLS;
  the VM already ran the previous goal's daemon, which reports them.
- `LuCI (disabled)` is the new scenario; the first version of its browser script
  clicked the Check MQTT button, which a disabled publisher does not show. The
  script was corrected and the scenario run again on its own: the page serves the
  `settings.js` of the source tree, the saved `mqtt_enabled=0` with a host and no
  `mqtt_tls` shows TLS off with the plain notice and no TLS fields after the
  publisher is ticked on, and nothing is saved.

New scenario in the harness: a listener on the broker host sends a TLS record
header and then a byte a second for three minutes; `--mqtt-status` on the router
with TLS on returned after 9.3 s with `tls_handshake: failed` and "MQTT TLS
handshake timed out". The upgrade, TLS cases, reload, broker loss, last will,
clean stop and 100 s keepalive scenarios passed as before. The VM was left on
the candidate daemon (`e2e4f407`), with its MQTT options removed and the
configuration equal to the original.

### Known limits

- The dribble scenario was not run against the old daemon on the VM; the unit
  test that hangs with the old code is the evidence for that.
- A section that LuCI saved with the publisher off after this release has no
  `mqtt_tls` either, so ticking it on later shows TLS off and the user turns it
  on; the daemon would run plain MQTT for it too.
- Only x86_64 was run; the AArch64 packages were built and verified.

## Changes after RC27 r320/r128: restart limits and deadline anchors (unreleased)

These results cover two fixes found in a review: the restart limit of an Auto-Tune
measurement counted restarts that had used no traffic, and a deadline lookup could
write a file and give a job of an earlier boot an anchor of this boot. Nothing else
changed; the deadline of a job is still counted from its admission.

### Source gate and packages

- Source gate 187, 57 commands: Full (1,834) and Lite (407) Rust tests, formatting,
  clippy, LuCI JavaScript and TypeScript checks, real-UCI tests; the hash of the
  sources before and after the run did not change.
- Daemon and LuCI packages for x86_64 and aarch64_cortex-a76 built from the same
  gate and verified (payload, metadata, ELF, overflow checks on).

### What changed, and the test that fails without it

- A measurement loses its basis when a route or runtime resets. Before the load
  starts that costs no traffic, but it was counted with the losses after the load
  began against one limit of 8, never reset, so a flapping route could end a run
  before any load. `MeasurementRestarts` keeps two counts: after the load (limit
  8, never reset) and before it (limit 64, reset when a capture is admitted). The
  two tests were run against a build where the early losses used the old counter:
  both fail.
- `JournalStore::deadline` is called on every event-loop pass. For a job without
  an anchor file (admitted by a release before anchors) it wrote the anchor from
  the remaining wall-clock time and stamped it with the current boot ID, so a job
  of an earlier boot was taken for one of this boot. Such jobs of this boot are now
  anchored once when the store opens (`anchor_jobs_without_deadline`), the lookup
  only reads, a job of another boot without an anchor is refused with
  `operation-deadline-from-another-boot` and gets none, and a lasting failure is
  remembered instead of re-reading the disk every time. A poisoned cache lock is
  recovered instead of skipped. Two journal tests cover the open-time anchor, the
  read-only lookup and the other-boot refusal.

### VM acceptance (x86_64, OpenWrt 25.12)

Candidate daemon `ad32c7c5` (Full and LuCI APK of gate 187) installed over the
previous one, backend `speedtest-go` r6:

- Speed Test (capped, current topology): completed in 24.6 s, 10,380 / 19,436
  kbit/s.
- Full Raw Auto-Tune: reached Review in 893 s, topology and fixed state restored,
  no pending UCI changes, service running, no panic or deadline error in the log.

### Known limits

- The two fixes concern paths that a healthy run does not enter (a flapping route
  before the load, a job left by an older release): the VM runs show that the
  normal paths still work, the unit tests show the fixed behaviour.
- The open question on the wait before admission (item 6 of the review: should it
  be bounded?) is unchanged: the duration is counted from admission by design.
- Only x86_64 was run; the AArch64 packages were built and verified.

## Changes after RC27 r320/r128: clean-up of duplicated code (unreleased)

These results cover a clean-up that is meant to change no behaviour: the boot clock
is read with one system call, the three eventfd drain loops and the two pidfd wait
loops are shared, four wrappers around `sys::effective_uid` and two hand-copied
signal numbers are gone, and a topology repeat is checked against the confirmed
pair before its traffic starts.

### Source gate and packages

- Source gate 188, 57 commands: Full (1,837) and Lite (409) Rust tests, formatting,
  clippy with undocumented unsafe blocks denied, LuCI JavaScript and TypeScript
  checks, real-UCI tests; the hash of the sources before and after the run did not
  change.
- Daemon and LuCI packages for x86_64 and aarch64_cortex-a76 built from the same
  gate and verified (payload, metadata, ELF, overflow checks on).

### What changed

- `sys::boottime_ms` reads `CLOCK_BOOTTIME`, the clock behind `/proc/uptime`, with
  one `clock_gettime` call; `identity::monotonic_boot_ms` no longer opens and parses
  a file on every event-loop and wait pass. A test compares it with `/proc/uptime`
  (within its 10 ms resolution) and checks that it never goes back. The parser and
  its tests are removed with the code they covered.
- `sys::drain_u64` replaces the same loop in `event_loop.rs`, `main.rs` and
  `bootstrap_runtime_owner.rs`; each caller keeps its own error messages.
- `process::wait_pidfd` replaces the loops in `calibration_service.rs` and
  `native_apply_lifecycle.rs`; the callers pass the names their messages used, so
  every message is the same. `open_stable_pidfd` stays in two places on purpose: a
  process that vanished is `None` in one and an error in the other.
- `sys::effective_uid` is called directly; `process.rs` uses `libc::SIGTERM` and
  `libc::SIGKILL`.
- A raw-topology repeat must keep the other direction at the rate of the confirmed
  pair. The replay refused anything else, but only after the repeat had been
  measured. The runner now checks the control with the same rule
  (`topology_repeat_follows_pair`) before it publishes it. The controls it builds
  always passed that rule, so a run behaves as before; the check turns a wrong
  control into an immediate error. No record or journal format changed.

### VM acceptance (x86_64, OpenWrt 25.12)

Candidate daemon `ffaf0b76` (Full and LuCI APK of gate 188), backend `speedtest-go`
r6, installed over the previous build:

- Service lifecycle (`goal-e-vm-service.py`, 33 checks: stop, start, restart,
  reload, SIGTERM and SIGINT to the controller and the calibration daemon,
  `--cpu-profile`, `--mqtt-status`, clean log and dmesg): all pass.
- Speed Test (capped, current topology): completed in 24.5 s, 9,719 / 19,747 kbit/s.
- Full Raw Auto-Tune: reached Review in 618 s with the options `throughput_first` and
  `bypass_download` (so the topology comparison ran), topology and fixed state
  restored, no pending UCI changes, service running, no panic or error in the log.

### Known limits

- The MQTT harness and the clock-step runs were not repeated: the clean-up does not
  touch that code beyond the shared clock read, which the Speed Test and Auto-Tune
  runs exercise.
- Only x86_64 was run; the AArch64 packages were built and verified.

## Changes after RC27 r320/r128: Automatic Rating on dual-stack routers (unreleased)

### The failure

On the owner's multi-WAN router (OpenWrt 25.12, IPv6 on both uplinks, daemon r320
with backend `speedtest-go` r5) every **Get rating** in automatic mode ended within
seconds with `speedtest-accounting-flow-registration-failed`, while two Full Raw
Auto-Tune runs on the same instance and route the same morning reached Review. The
error means that the `flow_fault` counter of the job's accounting table was not
zero: a packet of the backend's user was dropped because it was not part of an
accounted IPv4 flow on the measured uplink.

The difference between the two operations is the server choice. Auto-Tune
compares servers itself and always passes `--server`, as does a Speed Test with
a chosen server; the Automatic Rating, and a Speed Test without a chosen server,
let the backend pick the server. Only then does speedtest-go run its packet
loss analyzer, and that analyzer ignored `--source`: its TCP sampler was never bound,
and its UDP sender's binding failed because the bare address was resolved as
`host:port`. On a router with IPv6, Go prefers IPv6 (RFC 6724) for a server with
an AAAA record, so both sockets went out over IPv6. Read-only checks on the
router showed that most of the nearby servers have AAAA records (one of them, the
one Auto-Tune preferred, does not).

### Reproduction on the test VM

The VM has no IPv6 uplink, so one was emulated: a global IPv6 address and an IPv6
default route on the WAN through a gateway that never answers, and IPv4 to the
one nearby server without an AAAA record dropped so that the backend's own choice
fell on a dual-stack server.

- The backend run by hand (`--source <IPv4> --dns-bind-source`, no `--server`)
  sent 1 IPv6 SYN and 146 IPv6 UDP packets to the server it picked. With
  `--server` the analyzer does not start (the server record has no host) and no
  IPv6 packet left.
- Automatic Rating with the packaged r6 backend: failed after 29 s with
  `speedtest-accounting-flow-registration-failed`; the `flow_fault` counter of
  the backend table grew from 4 to 163 packets while it ran.
- The same rating with the patched backend: completed in 55 s (grade A+, 36
  samples per direction), `flow_fault` stayed 0.
- A capped Speed Test without the emulation, with the patched backend: completed
  in 24 s as before.

The patched backend for these runs was a static x86_64 build of the patched
source (the SDK build is pending, see below); the packaged backend was put back
afterwards and its hash checked.

### What changed

- Patch 140 of `speedtest-go` binds the analyzer's TCP sampler and UDP sender to
  the `--source` IP (a literal only; nothing is looked up). With an IPv4 source
  both consider only the server's IPv4 addresses. Three offline tests check the
  binding, that a source that is not an address keeps the upstream behaviour, and
  that the dialers refuse an IPv6 target; all three fail on the unpatched code.
  The patch belongs to the unreleased revision 6, so the daemon's backend floor
  (`>= 1.7.10-r6`) also requires it.

### Known limits

- The accounting rules are unchanged: a backend packet outside the accounted IPv4
  flows still stops the measurement. Nothing in the daemon can tell from the
  counter which packet it was; the router checks above and the VM reproduction
  are the evidence.
- On the owner's router the fixed r6 backend (x86_64 APK, binary `ab007e6a…`) replaced
  r5 in one package transaction; the daemon (r320), the configuration, the boot and
  the running controller and calibration processes were unchanged. The first Get
  rating stopped after 30 s with `quiet-window-timeout`, before any measurement:
  LAN clients were uploading in bursts of up to 57 Mbit/s against a quiet limit of
  about 45 Mbit/s. The second completed in 51 s: grade A+, 36 samples per direction,
  the `flow_fault` counter of the backend table zero in all six samples taken while
  it ran, limits unchanged.

## Changes after RC27 r320/r128: refusals of test traffic counted by reason (unreleased)

A test's traffic is counted in temporary nftables tables, and a packet those
rules cannot account for is dropped. Until now every such packet went into one
counter, so three different causes ended with one code,
`speedtest-accounting-flow-registration-failed`. The tables now count the
reason: `fault_family` (not IPv4), `fault_unregistered` (IPv4 that is not an
accounted flow) and `fault_egress` (leaving through another device than the
tested uplink), with the codes `speedtest-accounting-non-ipv4-traffic`,
`speedtest-accounting-flow-registration-failed` and
`speedtest-accounting-wrong-egress`. LuCI explains each code in the Get rating,
Speed Test and Auto-Tune windows.

### Source gate and packages

- Source gate 190, 57 commands: Full (1,838) and Lite (410) Rust tests,
  formatting, clippy, LuCI JavaScript and TypeScript checks, real-UCI tests; the
  hash of the sources before and after the run did not change.
- Daemon and LuCI packages for x86_64 and aarch64_cortex-a76 built from the same
  gate and verified (payload, metadata, ELF, overflow checks on).

### Tests that fail without the change

- The rule batch has five counters with the owner comment `:flow-v2`; the
  IPv4-only refusal comes after the accounted flows and before the refusal of
  other families; the egress guard counts only `fault_egress`.
- The accepted-batch golden test folds the split back into the previous form
  (one `flow_fault` counter, no IPv4-only refusal rule) and still matches the
  accepted hashes of both route variants byte for byte: nothing else in the rules
  changed.
- The counter reader: each reason gives its code, several at once give the most
  specific, every refusal counter must be present, an old `flow_fault` counter
  is not accepted in a current table, mixed formats are rejected, and tables of
  the previous format (`:flow-v1`) and without refusal counters are still read.
  The permanent probes accept only the current format and have their own three
  codes. A mutation that reports the old single code, or that counts the egress
  guard as unregistered, fails these tests.

### Real nftables

The exact production batches were applied in a throw-away user and network
namespace on the host kernel (6.18, nftables 1.0.9), with synthetic traffic
through an isolated veth pair:

- Accounted UDP, ICMP and TCP traffic of the owner: `rx` and `tx` only, all
  three refusal counters zero.
- A `notrack` IPv4 packet of the owner: dropped, `fault_unregistered` 60 bytes.
- IPv6 to `::1` on another port than 53: dropped, `fault_family` 80 bytes.
- The egress guard pinned to a device that is not used: a packet out through
  the veth is dropped and counted in `fault_egress` (92 bytes); DNS to
  `127.0.0.1:53` through `lo` passes; another loopback port is `fault_egress`.
- The same for the probe tables (root with the reserved group), and nothing at
  all for root sockets of another group.

### VM acceptance (x86_64, OpenWrt 25.12)

Candidate daemon `57e0ee71` with the fixed backend r6. Each refusal was
provoked on purpose, and each case put the VM back:

| Case | How | Result | Log line of the job |
|---|---|---|---|
| dual-stack Get rating | emulated IPv6 uplink, the nearby IPv4-only server dropped | complete in 53 s, no refusal | — |
| not IPv4 | the same with the backend from before patch 140 | `speedtest-accounting-non-ipv4-traffic` after 28 s | 17,026 bytes not IPv4 |
| wrong device | capped Speed Test, a host route sends the server out through eth2 | `speedtest-accounting-wrong-egress` | 2,160 bytes leaving through another device |
| unregistered | capped Speed Test, `notrack` for the backend's user | `speedtest-accounting-flow-registration-failed` after 29 s | 1,920 bytes IPv4 without an accounted flow |

- Speed Test (capped, current topology): completed in 25.7 s.
- Full Raw Auto-Tune: reached Review in 875 s, topology restored, no refusal and
  no panic in the log.

### Known limits

- An Auto-Tune whose traffic budget watch notices a refusal still stops with
  `owned-traffic-accounting-failed`; the job's log line names the reason.
- Only x86_64 was run; the AArch64 packages were built and verified.

## Changes after RC27 r320/r128: service start while SQM restarts (unreleased)

### The failure

The upstream iface hotplug restarts a managed queue (`run.sh stop`, then
`run.sh start`) outside the lifecycle lock, and `start-sqm` writes the state
file last. At boot this happens when `wan` and `wan6` come up while
`S97cake-autorate` starts. A start that looked at the queue in that gap failed
(`managed SQM counter /sys/class/net/ifb4eth0/statistics/tx_bytes is missing`,
`managed SQM ingress redirect is not an exact single match`), and every later
start refused with `service-start-pending-generation-recovery-required` until
a stop or a reboot. rcS runs `boot()` once, so nothing retried: one of the
owner's routers ran without its controllers for seven days after such a boot,
and the test VM failed the same way after a reboot.

### Reproduction on the test VM (before the change)

Daemon `57e0ee71` (the build of source gate 190) and
`tools/goal-k-vm-start-race.py` (git-ignored): stop the service, start the
managed queue with the stock SQM init (as `S50sqm` does at boot), then start the
service and run the iface hotplug (`ACTION=ifup ... 11-sqm`: a stop and a start
of the queue) after a delay.

| Scenario | Failed | Errors |
|---|---:|---|
| one hotplug restart, 42 starts with delays from 0 to 3 s | 8 | 2 × counter missing, 4 × ingress redirect, 2 × readiness confirmation (`controller-batch-readiness-changed`) |
| a second start right after each failure, no reboot | 8 of 8 | `service-start-pending-generation-recovery-required` |
| reboot while a boot script restarts SQM back to back during `S97cake-autorate` | 5 of 5 boots | 2 × ingress redirect, 3 × counter missing; the second start failed every time |

The readiness failures came from a controller that briefly reported
`WAITING_OPERATION` while its batch was accepted; the lifecycle took that for a
failed start. The section on unsafe code above records the same code under CPU
load.

### What changed

- Both managed-SQM checks of the start wait for a real event and repeat the
  idempotent start action, at most 30 s each, instead of one look.
- The readiness confirmation waits when a controller leaves readiness during
  the acceptance of its batch or holds the global lock for its own SQM check;
  acceptance resumes where it stopped.
- A start that finds the pending batch of an earlier start that never became
  ready runs the ordinary Stop transaction first (`stopping the remains of an
  earlier start that did not complete`). No recovery record changes.
- `boot()` retries a failed start in the background, three attempts in all,
  15 s apart (`start failed at boot, retrying (2/3) in 15 s`, then
  `start failed at boot after 3 attempts`). The pause and the bound live in
  `--service-lifecycle boot-retry`.

### Tests that fail without the change

- SQM start: a start action that fails in a hotplug gap is repeated after the
  next event and the start succeeds; a lasting mismatch fails at the deadline
  with the instance and the reason, after one action per event (no polling).
- Readiness, on the real loop over a fixture `/proc`, status file, batch and
  `ubus`: the controller leaves readiness at each runtime check of the
  acceptance in turn, and the start is accepted every time; a global lock held
  shared by a controller is waited for. The previous code failed with
  `controller-batch-readiness-changed` and with `service lifecycle is busy with
  another runtime mutation`.
- Recovery: a pending start batch is stopped before the new start, an accepted
  one and an empty store are left alone, a failed stop is reported.
- `boot-retry`: three attempts at most, non-canonical numbers refused.
- `tests/init-boot-retry.test.sh` (Full and Lite): attempts and retries for 0 to
  3 failures and for a disabled service, the log lines, no `sleep` in the
  shell; and, under BusyBox ash with a stand-in that takes the lock like
  `procd_lock`, that a retry never waits for the lock inherited by the loop or
  by its `logger`.

### Source gate and packages

- Source gate 192, 58 commands (the new shell test is one of them): Full (1,872)
  and Lite (444) Rust tests, six new in each, formatting, clippy, LuCI checks,
  shell tests and real-UCI tests; the sources did not change during the run.
- Daemon and LuCI packages for x86_64 and aarch64_cortex-a76 built from that
  gate and verified (payload, metadata, ELF, overflow checks on).
- Gate 191 had accepted the same Rust code with the first version of `boot()`.
  On the VM its retry loop kept `procd_lock` through the `logger` at the end of
  its pipeline: the retried start, and even `status`, waited for the lock until
  a reboot. The descriptor is now closed before the pipeline exists, and the
  BusyBox case of the shell test fails on the first version.

### VM acceptance (x86_64, OpenWrt 25.12)

Daemon `c4683636` from gate 192 with its packaged init script:

| Scenario | Before | After |
|---|---|---|
| one hotplug restart during the start, the 29 delays of two of the runs above | 6 failed | 0 failed; a start that met the restart waited up to 2.0 s |
| ten back-to-back restarts during the start, 5 delays | — | 0 of 5 failed; the start waited up to 5.0 s |
| ingress redirect removed for longer than the wait | — | the start fails after the 30 s wait with `managed SQM did not reach its exact start postcondition for primary: managed SQM ingress redirect is not an exact single match` and leaves its batch pending; with the redirect back, the next start, without a reboot, stops the remains and runs in 1.3 s |
| reboot, SQM restarted back to back during `S97cake-autorate` | 5 of 5 down | 10 of 10 running, controller up 15 to 18 s after boot |
| reboot, global lock held for the first 10 s of `S96` | — | 2 of 2 running after one retry, controller up 30 s after boot |
| plain reboot | — | 5 of 5 running, controller up 14 to 15 s after boot |

- Service checks: 33 of 33 before and after the reboots.
- Speed Test (capped, current topology): completed in 28.6 s.
- After back-to-back SQM restarts the running controller stays `HEALTHY`; while
  the restarts last it reports `download-device-missing` and similar runtime
  errors, as before.
- The VM configuration was the same at the end; the test fixtures were removed.

### Known limits

- A reload (Save & Apply) still starts a changed queue with one check; a hotplug
  restart at that moment can fail the reload. That path is not part of this
  change.
- A start that fails at boot three times stays stopped until it is started by
  hand; the log says so.
- A busy global lock during readiness raises no event, so it is retried every
  second, within the 90 s readiness limit.
- Only x86_64 was run; the AArch64 packages were built and verified. The
  owner's router still runs the release daemon.
