# Testing and Observed Results

This page separates reproducible validation from product claims. Internet
speed tests depend on the selected server, routing, client, and background
traffic; the measurements below characterize one test setup and are not a
universal benchmark.

## Test layers

Changes should be checked at four layers:

1. Rust unit tests and format/check gates.
2. LuCI JavaScript tests plus shell and JSON syntax checks.
3. Package builds and dependency-only installation into an empty APK root for
   every release architecture.
4. A router integration test that verifies live CAKE qdiscs, IFB ingress
   redirection, daemon state, reflector responses, and connectivity before and
   after load.

Transport-aware acceptance additionally verifies that clean ICMP cannot approve
growth while loaded HTTP/TCP delay is above target, no search candidate crosses
the calculated floor, `quality_limited` appears when the safe floor prevents the
target, old five-column graph history remains readable, and all new history
stays in `/var/run`.

RC8 requires deterministic coverage for the native transport RTT contract:
DNS/handshake warm-up exclusion, route-bound sockets, persistent connection
reuse, symmetric outlier removal, trusted/untrusted backends, 20-sample p5
idle and per-direction p90 loaded windows, CPU/load-phase rejection, and two
confirmed bad windows before optional control. Measurement-only mode must leave
all CAKE rates unchanged. Detected-grade tests cover the 2 ms noise clamp,
worse-of-download/upload selection, `PARTIAL` one-direction evidence,
bidirectional exclusion, route-change staleness, and `CURRENT`/`LAST KNOWN`
lifecycle. Incomplete and partial attempts must not replace `LAST KNOWN`. Graph
acceptance checks the proportional RAM tiers, critical-memory suspension,
shared per-instance budget,
streaming compaction, bounded history paging, vertical WAN cards, grade-event
hover details, and fixed non-scrolling axis labels.

RC9 additionally requires replay coverage for passive routed-traffic phase
detection: the two-second rolling mean, 60/40 hysteresis, one-second direction
hold, 1.5-second dropout, DL/UL dominance, bidirectional exclusion, and route
reset. Helper tests cover automatic completion, guided cancellation, readiness
failure, interface locking, and cleanup. Installed-browser acceptance must
exercise the `Get rating` modal and progress, complete a real shaped automatic
rating without changing CAKE, and verify safety-floor scaling, scrollbar-gutter
follow mode, hover rating phases/counts, vertical multi-WAN cards, and 390 px
mobile layout.

RC6 added a second disposable-router gate: two nftables mwan3 members, distinct
CAKE/IFB pairs, per-member ICMP and HTTP/TCP probes, router-side speed tests,
Full Auto-Tune isolation, failover/failback, and route relearning. Production
deployment is permitted only after both the original single-WAN safety gate
and this Multi-WAN gate pass.

Release offline bundles must be installed with networking disabled into an
empty APK root. Published assets must then be downloaded again and validated
against the published `SHA256SUMS`.

RC13 adds deterministic UI and calibration gates: clean package config must
contain no `cake_autorate` section; mandatory Status columns cannot be hidden;
saved optional columns and Reset default must survive polling; 390 px Status
must render cards while desktop remains aligned to the LuCI content container.
Optional columns may scroll only inside their table wrapper. Graph event labels are
tested with clustered route/state/grade/DL/UL changes and must occupy two
lanes on both synchronized charts. Re-run Auto-Tune must prefill the selected
instance and stage no change before Review. Background tests cover strict
stop, quiet retry, cancel, moderate conservative continuation, unusable
direction retention, and the invariant that a low-confidence result never
raises a confirmed max or cap.

RC14 additionally requires that a complete grade moves only to `LAST KNOWN`.
With no active episode, `CURRENT` must be null in status JSON and render as
`WAITING FOR DATA`; route changes and cancelled captures must not resurrect a
previous complete grade as current. Partial or incomplete attempts remain
eligible for the current-attempt slot but never replace `LAST KNOWN`.

## Anonymous WAN comparison

### Setup

- OpenWrt 25.12.5 x86_64 router on a nominal symmetric 1 Gbit/s PPPoE service.
- A WSL2 LAN client generated traffic through the router; the router itself did
  not run the speed test.
- `speedtest-go` v1.7.10 used one pinned nearby server for every mode.
- A separate one-second ICMP stream ran concurrently from the client.
- The router was monitored for actual CAKE rates, IFB presence, daemon state,
  and multi-WAN tracker state throughout each run.
- Hostnames, private/public addresses, ISP identity, and speed-test server
  identity have intentionally been omitted.

### Results

| Mode | Download | Upload | Concurrent ICMP | Controller observation |
|---|---:|---:|---|---|
| CAKE at 900/860 Mbit/s with autorate enabled | 837.5 Mbit/s | 799.6 Mbit/s | 0% loss, 2.59 ms average, 11.55 ms maximum | Autorate remained at 900/860 for the complete run |
| Fixed CAKE at 900/860, run 1 | 822.9 Mbit/s | 795.0 Mbit/s | 3.3% loss (one packet), 2.75 ms average, 9.29 ms maximum | qdiscs remained fixed |
| Fixed CAKE at 900/860, run 2 | 838.7 Mbit/s | 803.9 Mbit/s | 0% loss, 4.76 ms average, 14.78 ms maximum | qdiscs remained fixed |
| SQM and autorate fully disabled | 928.4 Mbit/s | 933.1 Mbit/s | 7.5% loss, 3.81 ms average, 12.41 ms maximum | WAN remained physically online |

The unshaped run gained roughly 90-130 Mbit/s of application throughput but
lost three of forty concurrent ICMP packets. The speed-test backend also
reported download loaded latency of 28 ms average and 212 ms maximum in that
run.

### Interpreting speed-test loaded latency

The speed-test backend's own loaded-latency result varied substantially even
with identical fixed 900/860 CAKE qdiscs: one run reported 5/8 ms download/
upload averages, while a later run reported 26/21 ms and a 210 ms download
maximum. The independent concurrent ICMP stream did not show a corresponding
210 ms spike. This indicates server/path/test variation and is why no tuning
decision should use that single backend field alone.

Use several signals together:

- independent latency and packet loss during saturation;
- live `tc qdisc` rates and counters;
- valid reflector samples and delay deltas;
- CPU headroom;
- repeated tests at different times; and
- confirmation that the physical WAN and policy tracker stayed online.

### Conclusions from this setup

1. CAKE/SQM was worthwhile: disabling it recovered the last part of headline
   throughput but introduced measurable packet loss under saturation.
2. Autorate and fixed CAKE performed equivalently while capacity was stable,
   because autorate made no rate change during the sample.
3. This does not make autorate unnecessary on a variable link. It shows that
   the controller should be judged during actual capacity changes, not merely
   by comparing two stable 20-second speed tests.
4. A useful variable-link configuration is a conservative proven starting
   maximum, realistic worst-case minimums, and bounded ceiling caps below the
   physical interface rate. The controller can then reduce rates quickly under
   delay and explore upward only during sustained clean load.

## RC12 synchronized router/client rating check (2026-07-14)

An x86_64 Multi-WAN router and a browser client behind its active backup uplink
were measured during the same LibreQoS test. Names, addresses, and provider
identity are omitted. The guided capture measured background traffic first and
used the current 108/14.5 Mbit/s CAKE limits as its directional references.

| Observer | Overall | Download | Upload | Throughput |
|---|---|---|---|---|
| LibreQoS browser | C | C, +117.5 ms | B, +43.7 ms | 87.9/3.8 Mbit/s |
| Router native WebSocket RTT | C | C, +112.43 ms, 135 samples | B, +42.12 ms, 22 samples | observed from the same routed test |

The two observers use different probe streams, so exact RTT percentiles are
not expected to be identical. Their overall class, per-direction classes, and
delay magnitude agreed. The daemon committed the clean router result as
`LAST KNOWN C`; a later incomplete passive window remained `CURRENT` and did
not replace it. A separate automatic attempt encountered simultaneous
opposite-direction background traffic, returned an explicit contamination
error, and did not publish a rating.

Before RC12, clustered reflector output could consume the same counter delta
twice or divide a small burst by a sub-millisecond interval, which often
classified only upload. RC12 shares one rate sample and coalesces reads closer
than 25 ms. A busy link that had remained at 0/20 idle baseline samples then
reached 20/20 in about ten seconds using the bounded one-second warm-up probe
interval and returned to its configured 15-second idle interval.

## Reproduction outline

Use a client behind the router and keep the test server constant:

```sh
speedtest-go --server <server-id>
ping -c 40 -i 1 <stable-reflector>
```

On the router, capture the actual shapers and status during the same interval:

```sh
tc qdisc show dev <wan-device>
tc qdisc show dev <download-ifb>
cat /var/run/cake-autorate/<instance>/status.json
```

Test at least these modes, restoring the saved configuration after each:

1. autorate plus managed SQM;
2. managed SQM with direction adjustment disabled; and
3. SQM and autorate disabled, only for a short controlled baseline.

Do not run an unshaped saturation test on a production router unless brief
packet loss and latency are acceptable. Confirm multi-WAN/policy-routing state
after every mode transition.

## Adaptive-ceiling acceptance scenarios

Deterministic tests cover:

- clean promotion of a new safe ceiling;
- immediate rollback and failed-bound creation after confirmed bufferbloat;
- midpoint convergence between safe and failed bounds;
- transient delay noise and eligibility grace;
- idle cancellation without a false failed bound;
- global response-gap abort;
- failed-bound expiry;
- stall reset; and
- independent asymmetric download/upload state.

See [ALGORITHM_MATH.md](ALGORITHM_MATH.md) for the equations and
[ADAPTIVE_CEILING.md](ADAPTIVE_CEILING.md) for safety invariants.

## Full Auto-Tune x86 safety gate (2026-07-12)

The r12 daemon and LuCI packages were installed on a disposable two-vCPU
OpenWrt 25.12.5 x86_64 VM without changing its existing UCI files. Full
Auto-Tune first refused the active WAN because an enabled instance already
owned it. After that owner was disabled through a temporary, uncommitted UCI
delta, the job created and later removed its CAKE/IFB validation shaper.

Two same-server raw samples were about 716-777/177-181 Mbit/s. Shaped attempts
retained only 53.5%/42.1% and 55.5%/72.5%, despite low ICMP latency and adequate
total CPU. The job therefore failed closed after its single correction. This
exposed two real portability details that are now covered by the lifecycle
test: BusyBox fping may omit its final summary when terminated, and public DNS
reflectors may individually rate-limit rapid ICMP. The parser now derives
reply/timeout loss when necessary, probes once per second, and uses median
per-reflector loss.

The original instance, 85/10 Mbit/s queues, configuration hashes, and empty UCI
delta state were restored. Playwright then opened the installed Full Auto-Tune
and Manual wizard paths, confirmed all three visual steps and safety notices,
and reported no TypeError or invalid-constructor failure.

## Variable-WWAN LibreQoS regression (2026-07-12)

This regression was added after a client-side run appeared to move from grade
C with autorate disabled to D with autorate enabled. The router was an ARMv8
OpenWrt 25.12.5 system on a genuinely variable WWAN link. A headless Chromium
client entered through an SSH SOCKS tunnel, so every browser request exited the
tested WWAN interface. Identifying addresses, carrier, and hostnames are
omitted.

| Mode | Grade | DL / UL | Scored loaded increase | Bidirectional increase |
|---|---:|---:|---:|---:|
| Autorate + CAKE, run 1 | C | 100.5 / 13.1 Mbit/s | +157 ms | +45 ms |
| Fixed CAKE at 114.5 / 15.8 Mbit/s | C | 103.6 / 12.7 Mbit/s | +166 ms | +85 ms |
| SQM fully disabled | D | 138.5 / 19.7 Mbit/s | +234 ms | +398 ms |
| Autorate + CAKE, repeated | C | 105.6 / 12.5 Mbit/s | +192 ms | +60 ms |
| Autorate with a temporary 90 / 12 Mbit/s start | D | 81.4 / 10.3 Mbit/s | +203 ms | +62 ms |
| Autorate + CAKE with diagnostic HTTPS probe | C | 104.3 / 14.1 Mbit/s | +179 ms | +58 ms |

The reported C-to-D direction was not reproducible as a deterministic autorate
regression. Completely unshaped service was clearly worse, while autorate and
fixed CAKE were close. The repeated autorate results ranged from +157 to
+192 ms, and merely lowering the rate produced +203 ms. A single C or D close
to the 200 ms boundary is therefore not a sufficient tuning signal on this
link.

The synchronized daemon trace exposed the actionable issue. During the
+192 ms browser run, the controller's six ICMP reflectors saw only 11.3-54.2 ms
RTT and at most 22.4 ms EWMA delay growth. CAKE download ranged from 86.8 to
114.5 Mbit/s and upload from 12.3 to 15.8 Mbit/s; CPU peaked at 38.6%. The
controller was functioning and classified bufferbloat, but its ICMP signal was
far more optimistic than loaded TCP.

A small HTTPS request to the same Cloudflare path provided the missing signal:
idle requests were normally 230-350 ms including process, DNS, TCP, and TLS
overhead, then rose repeatedly to 450-610 ms during the download phase while
ICMP remained comparatively clean. This is consistent with carrier/path ICMP
prioritization, not duplicate byte accounting or reversed directions.

Consequences:

1. Do not solve this case by blindly reducing the starting rate; the controlled
   90/12 Mbit/s trial lost throughput without improving the grade.
2. This historical pre-RC8 experiment added an idle and loaded TCP/HTTPS signal
   beside fping. At that time either delta above 100 ms failed closed and the
   implementation timed `uclient-fetch`; current Full Auto-Tune uses the native
   probe and the per-instance UCI target (30 ms by default).
3. Runtime autorate therefore uses a non-prioritized HTTP/TCP signal in addition
   to ICMP. In structured Multi-WAN mode the HTTP client is executed through the
   selected nftables mwan3 member; main-route mode still verifies that the
   target is the active default route.

The updated Full Auto-Tune gate was then exercised on the same ARM router. Two
raw samples proposed a variable-link base of 93.2/18.7 Mbit/s. Shaped attempt 1
saw only +31.5 ms ICMP growth but +240 ms TCP/HTTPS growth and failed. Its sole
bounded correction proposed 88.5/17.8 Mbit/s; attempt 2 still saw only +25.9 ms
ICMP growth versus +200 ms TCP/HTTPS growth and failed. Throughput retention was
67.6%/72.3%, loss 0%, and CPU 41%. The job returned
`configuration_written=false`, removed its temporary IFB/qdiscs, and preserved
the original UCI files. This is the intended fail-closed behavior for the exact
carrier asymmetry that motivated the regression.

The router was restored byte-for-byte to its saved cake-autorate and SQM
configuration after the tests, with the original instance running and no UCI
deltas left behind.

## RC6 Multi-WAN acceptance gate (2026-07-13)

The RC6 x86_64 packages were installed on a disposable OpenWrt 25.12.5 router
with two native nftables mwan3 members. Identifying addresses, names, and ISP
details are omitted. The primary and backup resolved to separate Ethernet
devices and policy tables, but both happened to share one upstream public NAT
address. This deliberately verified that public IP is supporting evidence, not
the sole route discriminator.

Two enabled autorate instances owned distinct SQM sections, CAKE root qdiscs,
download IFBs, source addresses, fwmarks, tables, reflector pools, transport
baselines, quality state, and adaptive ceilings. The normal policy produced
`ACTIVE` for the primary and `STANDBY` for the backup. Disabling only the
primary mwan3 member produced `OFFLINE` for that instance and `ACTIVE` with a
100% policy share for the backup. The offline pinger remained stopped and did
not add reflector offences. Restoring the member produced a bounded
`LEARNING` interval, rebuilt its baseline even though the controller had been
idle, and returned to `ACTIVE`; the backup returned independently to
`STANDBY`.

Forced ICMP, HTTP/HTTPS, built-in HTTP throughput, and speedtest-go checks used
the expected member/device/source/fwmark/table for each instance. A backup
speedtest-go sample reported approximately 51.8/5.7 Mbit/s and paused only the
backup daemon/SQM. The primary process and qdiscs remained present. Aggregate
WAN counters were never used to calculate the test rate.

Full Auto-Tune was then run on the backup. The first raw sample selected one
speedtest-go server; the second raw sample and both shaped validation attempts
reported the same server ID after the RC6 job-local pin fix. The candidate
failed closed for real quality evidence—about 28.3% median reflector loss and
120 ms loaded HTTP/TCP increase—rather than route/server mismatch. It returned
`configuration_written=false` and restored both autorate processes and both
original qdisc pairs.

The automated release gate passed 72 Rust tests, init/SQM conflict tests,
scheduler and Auto-Tune lifecycle tests, speed-test route tests, four LuCI
JavaScript suites, shell syntax, ACL JSON parsing, and `git diff --check`.
Playwright then exercised installed Status, Settings, and Graphs without a
constructor/page exception. It confirmed member labels (`logical -> device`),
both package versions, `BASELINE READY / Waiting for loaded traffic` at the
intentional 50% evidence stage, exact hover values, and fixed chart axes:

| Viewport | Left Y label, start/end | Right Y label, start/end |
|---:|---:|---:|
| 1440 px | 146 / 146 px | 612.47 / 612.47 px |
| 480 px | 21 / 21 px | 375.47 / 375.47 px |

The data/timeline scrolled over more than 2,000 px in both layouts while these
coordinates remained fixed. This is the acceptance condition for the RC6
graph-scale regression.

## RC7 detected-grade and RAM-history acceptance gate (2026-07-14)

This is retained as a historical UI/RAM-history gate. It did not establish
browser-rating parity: RC7 still timed whole `uclient-fetch` executions. The
network-RTT defect and its RC8 replacement are documented in the next section
and in [TRANSPORT_QUALITY.md](TRANSPORT_QUALITY.md).

RC7 was first installed on a disposable OpenWrt 25.12.5 x86_64 router, then on
an x86_64 two-uplink nftables-mwan3 router and an ARMv8 variable-WWAN router.
The existing `cake-autorate`, SQM, network, and (where present) mwan3 files were
hashed before and after each upgrade. Every hash remained unchanged. Existing
CAKE rates, PPPoE overhead, routes, source addresses, policy tables, active /
standby state, and external-path selection also remained unchanged.

The exact release artifacts used for the gate were:

| Artifact | SHA-256 |
|---|---|
| x86_64 daemon APK | `a69faf03c8905579ff465a99b1628776e2516f8185fd9f692db2e04a6ef9753c` |
| aarch64_generic daemon APK | `b41a73730cdee070e01793438f99346b81c7ed6a4bcf5c02153cb27726380661` |
| noarch LuCI APK | `afedbabe92c993ae61d9681834c89795a11a0d002ec1af0536c0e21fc280bfc6` |

All runtime status files parsed as JSON and reported daemon version
`1.0.0-rc.7`. The two-uplink router retained independent route identities and
histories for its active primary and standby backup. The WWAN router retained
its main-route identity and custom rates. The Status page displayed the live
detected-rating lifecycle and a retained completed-result slot; before any
result had completed, the latter explicitly reported that no completed rating
existed rather than implying a second learning cycle.

RAM budgeting was checked at two materially different memory sizes. A
disposable router with roughly 0.7 GiB available RAM exposed a 16 MiB safe
maximum and selected a 4 MiB automatic budget. Larger routers exposed the
100 MiB hard maximum; Auto selected 16 MiB total, split into 8 MiB per enabled
history on the two-uplink system. Presets above the effective safe maximum were
disabled in LuCI. History remained under `/var/run`, the helper returned
bounded page/stat results, and every CSV row used the extended grade/state
schema.

Authenticated Playwright checks ran against the installed package on all three
routers. They verified:

- one vertically stacked graph card per uplink;
- synchronized latency/CPU and download/upload canvases;
- Y-axis labels remaining fixed during horizontal timeline scrolling;
- exact RTT, CPU, DL, UL, floor, state, and grade values on hover;
- current/retained detected-rating labels and the empty last-known state;
- dynamic RAM preset disabling, usage, per-instance share, and history span;
- no application console or page exception after authentication; and
- no horizontal overflow at a 390 px mobile viewport.

The local release gate passed 78 Rust tests, strict Clippy, Rust formatting,
init/SQM conflict tests, scheduler and Auto-Tune lifecycle tests, speed-test
routing tests, the graph-history helper test, four LuCI JavaScript suites,
shell and JavaScript syntax checks, ACL JSON parsing, and `git diff --check`.

## RC8 native transport RTT acceptance gate (2026-07-14)

RC8 was installed first on the disposable x86_64 router, then on the same
two-uplink x86_64 nftables-mwan3 system and ARMv8 variable-WWAN system used by
the earlier gates. The final artifacts were rebuilt after the outlier-removal
regression test was added:

| Artifact | SHA-256 |
|---|---|
| x86_64 daemon APK | `9b880ab804ee73f949248ba3097892ab8257a3794d93393820d60bbda6fb4145` |
| aarch64_generic daemon APK | `4d6064c24cdb1b71d5df15fba605a2f7e1135e92d87f8ff31de34851ad37a1c0` |
| noarch LuCI APK | `e8dbd3402a8398bc3666138c435c25740616401f80617f6b2650a9b9d90e5375` |

Each offline repository indexed 65 APKs. With networking and package scripts
disabled, all 65 installed into a fresh architecture-specific root. Bundle
SHA-256 values are
`9ebfa2f6a36f33a7f55714ae3386dd68d4142e28f1f837a1f122647e3462a6bb`
for x86_64 and
`a53966c4358430ae4f41043b9ed2a4c0ffc3e2a850f306f00ed64b7f2c44e5f7`
for rockchip/armv8. The x86 installer was then run from its extracted bundle on
the disposable router; it used only `packages.adb`, created a dated RC8 backup,
left the final binary installed/running, and preserved all four UCI hashes.

Before RC8, process-timed HTTP baselines on the real routers were roughly
229-389 ms. One retained result called a link C from a 296 ms idle baseline,
despite a simultaneous browser-side test reporting A+. The number included
process, DNS, TCP/TLS, and remote HTTP time; it was not comparable to browser
network RTT.

The final RC8 persistent WebSocket probes instead measured these anonymized
paths:

| Path | WebSocket network RTT | TCP-connect comparison | Learned idle p5 |
|---|---:|---:|---:|
| primary PPPoE | about 0.8-1.5 ms | about 0.7-0.9 ms | about 0.8 ms |
| standby Ethernet uplink | about 19-31 ms | about 17-24 ms | about 19 ms |
| variable WWAN | about 17-28 ms | about 16-24 ms | about 19 ms |

Every native batch used four sequential observations and reported connection
reuse on its second invocation. A deliberately isolated observation was
removed symmetrically: status/CLI reported `discarded=1`, and only the three
accepted raw RTT values reached the p5/p90 trackers. DNS and a simulated
120 ms initial handshake were excluded by deterministic tests.

The primary and standby instances retained separate device, source address,
fwmark, routing table, public-address evidence, baseline, and persistent
connection. Both reached `baseline_ready` after at least 20 accepted idle RTT
samples. The ARM main-route instance did the same. Status reported
`network_rtt_v2`, trusted `websocket`, connection reuse, and
`transport_rtt_p90_loaded_minus_p5_idle_v2` throughout.

`transport_controller_enabled` remained false on all upgraded instances.
Primary CAKE stayed at 900000/860000 kbit/s, backup CAKE at 108000/14500
kbit/s, and WWAN CAKE at 114515/15773 kbit/s. Saved cake-autorate, SQM,
network, and mwan3 hashes were identical before and after every install; the
disposable router's temporary acceptance settings were restored byte-for-byte.
The production mwan3 service was never restarted.

During the final observation window, the physical PPPoE interface remained up
while the router's pre-existing mwan3 ICMP tracker intermittently lost two of
its three primary-member targets. A route-bound native TCP probe still
succeeded, and the standby member remained online. RC8 consequently moved only
the affected instance through `OFFLINE`, `LEARNING`, and `ACTIVE`, invalidated
its stale baseline after each route-state change, and left the standby
instance's baseline intact. This is expected failover/relearning behavior and
also explains why a live primary instance may temporarily show zero learning
progress during real tracker churn; it is not a return of the RC7 process-time
measurement defect. No mwan3 configuration or service state was changed for
this observation.

Authenticated read-only Playwright acceptance passed on both production
routers. The disposable run additionally changed one Quality field, used the
modal `Save`, applied it, reopened the modal to prove persistence, restored the
old value, and applied again. Status tooltips showed backend/trust/reuse,
controller state, raw/discarded samples, and rejection reason. Graph cards were
vertical per WAN, both canvases shared one timeline, and fixed Y labels did not
move with horizontal scrolling. No LuCI application page exception remained.

The final local gate passed 82 Rust tests, strict Clippy and formatting, six
shell lifecycle/routing suites, four LuCI JavaScript suites, package builds for
x86_64 and aarch64_generic, shell/JSON/diff validation, and the three live
router checks above.

## RC9 passive rating and graph acceptance gate (2026-07-14)

RC9 was built for x86_64 and rockchip/armv8, installed first on the disposable
x86 router, and then on the same production Multi-WAN and ARM variable-WWAN
routers. The project APKs used for the live gate were:

| Artifact | SHA-256 |
|---|---|
| x86_64 daemon APK | `388f6a0b35c8e01d11aee936e245e8491a1aef5b004dc6e6fb904a03dcdf4011` |
| aarch64_generic daemon APK | `98ec7832bf43458377710152e1092db7a43060d4d7ed827736dd626e45a2bd2f` |
| noarch LuCI APK | `ee38e03158d4cef9f1aa20f17144b6953abaaf38a36d49b825150cb7baab2ba7` |

The disposable router retained its exact cake-autorate and SQM hashes and its
85/10 Mbit/s CAKE pair across installation. A temporary measurement-only
WebSocket setting reached a trusted 20/20 idle baseline. Playwright opened the
new per-instance action, selected guided capture, observed live baseline,
DL/UL, phase, and load progress, cancelled it, and confirmed that the runtime
marker disappeared. CAKE and the SQM hash were unchanged.

Playwright then ran the complete automatic path with SQM and autorate left
enabled. The helper collected 61 download and 53 upload raw RTT samples and
finalized `A+`: idle p5 was 1.715 ms, loaded p90 was 3.377/3.348 ms, and both
directional increases entered the 2 ms noise clamp. The result was stored as
`CURRENT/final`; the helper reported completion without applying any rate. CAKE
remained exactly 85/10 Mbit/s. The temporary UCI file was then restored to its
original hash.

The browser gate caught two genuine integration defects before release. LuCI
had serialized Boolean false as `disabled="false"`, which still disables an
HTML button; both `Get rating` and `Start rating` now omit the attribute when
enabled. A stable scrollbar gutter also made the old latest-edge calculation
15 px too large, causing `Latest` to leave follow mode immediately. The graph
now computes the actual maximum from the gutter-aware viewport width, with a
JavaScript regression test.

More than 300 real one-second RAM rows were rendered. The desktop timeline grew
to 1450 px inside a 1135 px viewport; both canvases had identical widths. Fixed
Y overlays did not move while the timeline scrolled, hover reported exact
rating phase and DL/UL counts, manual scroll-back survived polling, and
`Latest` returned to the actual right edge. Safety floors were off initially;
enabling them changed a quiet 10 Mbit/s scale to 50 Mbit/s as expected. The
same checks passed at 390 px without horizontal page overflow.

On the production Multi-WAN router, package installation preserved the
cake-autorate, SQM, and mwan3 hashes. The primary 900/860 Mbit/s CAKE pair and
backup 108/14.5 Mbit/s pair were unchanged; lifecycle remained
`ACTIVE`/`STANDBY`, route identities and external-address evidence were
unchanged, and mwan3 was never restarted. Read-only Playwright confirmed two
separate vertically stacked cards, correct independent state/floors, two
synchronized canvases each, and no page or console exception.

The ARM router likewise retained its cake-autorate/SQM hashes, its existing
WWAN CAKE rates, route identity, and `ACTIVE/RUNNING` lifecycle. Its installed
daemon reported `1.0.0-rc.9`, and the HTTP LuCI Status/Get rating/Graphs/mobile
gate completed without an application error.

The local gate passed 88 Rust tests, strict Clippy and formatting, six shell
lifecycle/routing/helper suites, four LuCI JavaScript suites, shell and
JavaScript syntax, ACL JSON parsing, `git diff --check`, both SDK builds, and
the three installed-router checks above.

Each RC9 offline repository indexes 65 APKs. Both project packages and all
required dependencies were then installed with networking disabled into empty
x86_64 and aarch64_generic roots; APK selected 62 required packages on each
architecture and completed without a feed lookup. The release installers pass
`sh -n`, the two archives each contain the installer, `packages.adb`, and all
65 APKs, and the final release payload hashes are:

| Artifact | SHA-256 |
|---|---|
| x86_64 installer | `626722ec88db229d1388470d14f52d57a2e9f06737181ea39ad5a54d400b8c7c` |
| aarch64_generic installer | `5f46092a298742fe17bb0cb7f140da4e73b78448880f89cd3d4de16c92231747` |
| x86_64 offline bundle | `8615f596d659dfd09642d16ed2da95da35b0a104f71055a43477b46c682ef946` |
| rockchip/armv8 offline bundle | `4d627db78be016b5541a9343c6922dffb72f9c6f12c355e2208ecfd800fb85a0` |

The release `SHA256SUMS` covers these four files plus both daemon APKs and the
shared noarch LuCI APK. Published-asset verification is performed again from a
fresh directory after GitHub upload.

## RC10 background-aware rating acceptance gate (2026-07-14)

RC10 separates generated download and upload load, measures the idle traffic
already present on the link, and resets the directional counters for every new
`Get rating` job. The final project APKs used by the live gate are:

| Artifact | SHA-256 |
|---|---|
| x86_64 daemon APK | `aa4632f835b91e84814664f6ff44d54c51a6fe6defe52f2aac79c1ca61a75d74` |
| aarch64_generic daemon APK | `062062363d03cc3295d1701db072c855892e9211762253d25be44ce14bf35405` |
| noarch LuCI APK | `439e95d15f2b33803faf38342b53bd480b696316efd7868a577314d39d46ce86` |

The disposable x86 router first exposed the RC9 failure mode: during a shaped
download the reverse TCP acknowledgements occupied about 25% of a much smaller
upload CAKE rate and were incorrectly called background contamination. The
final detector waits until the requested direction is loaded, requires the
opposite direction to exceed its own CAKE-based boundary, and then permits a
bounded ACK ratio relative to the requested-direction traffic. Unit coverage
retains a real asymmetric 85/10 Mbit/s trace and a separate genuinely
contaminated trace.

The final automatic live run observed about 82.6 Mbit/s effective DL together
with 2.5 Mbit/s reverse ACK traffic without contamination. The download phase
collected 90 samples, the later upload-only phase collected 66, and the result
finalized as `A+` for both directions with a 2.128 ms loaded increase. The
helper did not change CAKE: root/IFB rates remained exactly 10/85 Mbit/s. Its
temporary transport settings and job files were removed, and the original
cake-autorate and SQM hashes were restored byte-for-byte.

The same packages were then installed on the two-uplink x86 nftables-mwan3
router and the ARMv8 variable-WWAN router. Their cake-autorate, SQM, and (where
present) mwan3 hashes were unchanged. The x86 primary/backup instances retained
`ACTIVE`/`STANDBY`, their distinct route identities, and 900000/860000 plus
108000/14500 kbit/s CAKE pairs. The ARM instance retained `ACTIVE`, its WWAN
route identity, and 114515/15773 kbit/s. Only cake-autorate was restarted;
mwan3 was never restarted.

Authenticated Playwright checks passed on the disposable and both production
routers. They opened Status and the expanded rating dialog, verified the RC10
daemon/LuCI versions and readiness messaging, and checked one vertically
stacked graph card per uplink. Every card retained two synchronized canvases,
fixed Y-axis overlays, exact rating phase/DL/UL data on hover, and a valid
390 px mobile layout. The disposable graph also showed the completed `A+`
episode. No LuCI application exception occurred.

The final local gate passed 96 Rust tests, strict Clippy and formatting, six
shell lifecycle/routing/helper suites, four LuCI JavaScript suites, both SDK
package builds, shell/JavaScript syntax, `git diff --check`, and all three live
router checks. The quiet-link timeout, background subtraction, independent
DL/UL capture thresholds, frozen candidate threshold, explicit phase
acknowledgement, ACK allowance, and true opposite-direction contamination all
have deterministic regression coverage.

Each RC10 offline repository contains 65 APKs and a newly generated
`packages.adb`. With networking and package scripts disabled, APK selected and
installed all 62 required packages into fresh x86_64 and aarch64_generic roots.
Both installers pass `sh -n`; both archives contain the platform installer,
index, and all 65 APKs. The remaining release payload hashes are:

| Artifact | SHA-256 |
|---|---|
| x86_64 installer | `a682a53b56cee3341b6d5eb9318fadf92060b442419c5576eae8009642d29df8` |
| aarch64_generic installer | `41835aff3c4fddf02ae18a78f7d5df8af7997e3cf8cb8482d61da7044cd111c1` |
| x86_64 offline bundle | `8955e6144d8ce6261e8a8392e89ba7d9382ceddd9be06fc16de47256dd669004` |
| rockchip/armv8 offline bundle | `113af86b542ed6e5e37b22c258584f206a7e34b63c118d84aa70204e0243b11b` |

The release checksum manifest covers these four files plus both daemon APKs
and the shared noarch LuCI APK.

## RC13 compact UI and safe recalibration gate (2026-07-14)

RC13 was built for x86_64 and rockchip/armv8 and installed first on a
disposable x86 router. A real purge/reinstall proved that the package default
contains only the global RAM-history policy: it created no autorate instance,
no managed SQM queue, and no daemon instance process. The saved test
configuration was then restored byte-for-byte.

Authenticated Playwright exercised the installed LuCI application at desktop
and 390 px widths. It verified that the four mandatory Status columns remain
visible, optional column choices and Reset survive a reload, desktop remains
aligned to the LuCI content container, and mobile renders cards without page
overflow. Re-run
Auto-Tune opened the selected instance with its route, queue, backend, and
rates prefilled. Edit showed the four topic tabs plus Advanced only when expert
options were enabled. Clustered LEARNING, route, rating, DL, and UL markers
used two non-overlapping lanes on both synchronized charts; fixed axis names
did not move with the timeline, and hover exposed the exact values.

The final packages were then installed read-only with respect to configuration
on an x86 nftables-mwan3 router and an ARMv8 variable-WWAN router. The x86
instances retained independent `ACTIVE`/`STANDBY` route state and their
900000/860000 plus 108000/14500 kbit/s CAKE pairs. The ARM instance retained
`ACTIVE/RUNNING` and its existing WWAN limits. cake-autorate, SQM, network, and
(where present) mwan3 configuration hashes were identical before and after;
mwan3 was not restarted. Playwright passed Status, Graphs, both responsive
layouts, hover, action ordering, and topic-tab checks on both devices without
a LuCI exception.

The x86 offline archive was copied to `/root/` on the disposable router,
extracted, and installed from its local `packages.adb` with `--no-network`.
The installer reported the exact RC13 daemon and LuCI versions, created a dated
backup, restarted the existing instance, and preserved the cake-autorate and
SQM hashes. Both repositories index 65 APKs; the ARM daemon itself was also
installed and exercised on the ARM router. Final release hashes are:

| Artifact | SHA-256 |
|---|---|
| x86_64 daemon APK | `f3b0d299aaeccebe43a57fbe000fcd7234fb672ee42418ec71a9891ddf157cb5` |
| aarch64_generic daemon APK | `1cfbf6c7a1717ffc5749987604f210d2486d3ec1f46dd1987cf12823e330a6fc` |
| noarch LuCI APK | `b547ba8d24bf97d010475c52d62110259829c480d718f743ba798f58453e7987` |
| x86_64 installer | `5350c2c478bd7390023bd7b7c758608709e5ceee3ca7bb76c125c6502c8a062c` |
| aarch64_generic installer | `99b8a228bf718bc95cd2875c7014abcaaf0c6b83e4fb2fc07f5ae61d107e0d37` |
| x86_64 offline bundle | `3138d86f313756cab0f1ea3e9e072e69ad0b2c3a2819bf5864cdcb62a03eb046` |
| rockchip/armv8 offline bundle | `043d2ca269604c48de9418ef7ae2655ff6202881d031e793ddc777dd9b5114de` |

The deterministic final gate comprises 103 Rust tests, strict Clippy and
formatting, all daemon and LuCI shell suites (including clean defaults and the
isolated Status-column commit helper), four LuCI JavaScript suites, shell/JSON
syntax, `git diff --check`, both SDK builds, checksum verification, the offline
installer run, and the three installed-router browser checks described above.

## RC14 Status geometry and rating-lifecycle gate (2026-07-15)

RC14 first reproduced the RC13 width defect with authenticated Playwright. At
2048 px the LuCI content container and application header were 1180 px wide at
x=434, while Status escaped to 2000 px at x=24. Equivalent escape was measured
at 1280, 1500, and 1920 px. The corrected page now remains exactly 1180 px and
aligned with the header at all four desktop widths. The compact four-column
layout uses the full container; enabling every optional column grows only the
inner table to 1770 px and leaves the page itself at the viewport width.

At 390 px Status switches to cards, has no page overflow, and keeps the Get
rating button and its readiness explanation in a vertical stack. The browser
gate caught and corrected an initial mobile overlap before the final LuCI APK
was built. Read-only production checks also opened Graphs after a polling
cycle: the two-uplink router rendered two vertically stacked cards and four
canvases, while the ARM router rendered one card and two canvases, without a
page or console exception.

The rating lifecycle was then exercised on the disposable router rather than
only mocked. Baseline learning reached 22 accepted samples, automatic Get
rating collected 50 DL and 57 UL samples, and the helper completed `A+` without
changing the 85/10 Mbit/s CAKE pair. Immediately after finalization the daemon
reported `quality_grade_state=baseline_ready`,
`quality_grade_current=null`, and `quality_grade_last_known.grade=A+` with a
complete, non-partial result. LuCI consequently rendered `CURRENT — WAITING FOR
DATA` and `LAST KNOWN — A+`. A genuinely new passive incomplete episode may
occupy CURRENT, but it cannot replace LAST KNOWN; starting or cancelling a
guided capture also cannot resurrect the old complete result as CURRENT.

The final packages were installed on the x86_64 nftables-mwan3 router and the
rockchip/armv8 router. cake-autorate, SQM, network, and (where present) mwan3
hashes were identical before and after. Only cake-autorate was restarted. The
x86 router retained independent `ACTIVE`/`STANDBY` members and route
identities; the ARM router retained `ACTIVE/RUNNING` and its existing WWAN
limits. Authenticated Playwright confirmed the RC14 daemon/LuCI version banner,
content-aligned Status at 1280/1920/2048 px, 390 px cards without overlapping
content, and the expected synchronized graph count on both devices.

Both offline repositories index 65 APKs. Dependency-only installation with
networking and package scripts disabled selected all 62 required packages in
fresh x86_64 and aarch64_generic roots. The x86 archive was then copied to
`/root/` on the disposable router and its installer completed with
`--no-network`, created an RC14 backup, and preserved the cake-autorate and SQM
hashes. Final release hashes are:

| Artifact | SHA-256 |
|---|---|
| x86_64 daemon APK | `51c9330de9cc626eb485bed16d07cb4526579c6ebbdc66ee943aa1a8af644b52` |
| aarch64_generic daemon APK | `86ab6f4c4d2a6d370ca174018fb56a505f9544c4fe76d8fa21bf8d78e42872e8` |
| noarch LuCI APK | `389db479b3468349d41ca08936803a90ee67ff02df6c45f420e1683cc8157f5b` |
| x86_64 installer | `08ff7d90f399a29fe141c1acf997e1f199d21f73b7ac4b07adc325211eb994c7` |
| aarch64_generic installer | `ddafcb3f0e84704f9b375142c8eb62100eb479640174e6344c15b2f9f809e5d9` |
| x86_64 offline bundle | `48bfc6efa196c28595a859d91d1ea037ff4d7248b3336944c6a5363411eb0034` |
| rockchip/armv8 offline bundle | `7d04f6cc934aa28e8630508623095efaefc94d37929c993c9061eda402bf3a65` |

The final deterministic gate passed 103 Rust tests, strict Clippy and
formatting, nine shell suites, four LuCI JavaScript suites, JavaScript/shell
syntax, ACL JSON parsing, `git diff --check`, both SDK builds, both empty-root
offline dependency installs, the `/root/` installer run, and all three router
browser checks above.

## RC15 graph-event and Autorate-navigation gate (2026-07-15)

RC15 was reproduced against the live multi-hour RAM history from the x86_64
Multi-WAN router before its daemon was restarted. The captured `wan_sqm`
history was 566,599 bytes and contained real `ACTIVE → OFFLINE → LEARNING →
ACTIVE` transitions separated by only seconds on a several-hour timeline. The
old two-lane renderer placed the third label into an occupied lane. The new
deterministic test maps the same timestamps to a narrow plot, verifies that
`LEARNING` is not emitted as a fake quality grade, clusters events within 12
screen pixels, retains the complete transition list for hover, and guarantees
that the bounded three-lane label layout never overlaps.

The noarch RC15 LuCI APK was first installed without restarting the daemon, so
Playwright exercised the corrected renderer against the original history.
Both synchronized charts used the same clustered markers. Three dense WAN
transitions occupied three distinct text rows, nearby backup-WAN rating events
were also separated, and the 390 px page had no horizontal overflow or browser
exception. After the full package upgrade and controlled daemon restart,
`wan_sqm` returned to `ACTIVE/RUNNING` and `wanb_sqm` to
`STANDBY/RUNNING`. The cake-autorate, SQM, network and mwan3 configuration
hashes were unchanged.

Installed Settings acceptance opened the real Edit modal on both the
disposable router and the Multi-WAN router. The **Autorate setup** selector had
exactly six groups with 11, 10, 8, 24, 29 and 20 rendered options respectively.
Every switch left the inactive option nodes in the same form, exactly one
tabpanel and one ARIA tab were active, a no-op modal Save parsed successfully,
and a fresh open retained all groups. At 390 px the selector was a two-column
grid with no intersecting buttons or body overflow. Configuration hashes on
the disposable router remained unchanged.

Both OpenWrt 25.12.5 SDK builds completed for x86_64 and
rockchip/armv8 (`aarch64_generic`). The noarch LuCI package was independently
built in both SDKs and produced the same SHA-256. Each offline repository
contains and indexes 65 APKs. With networking and package scripts disabled,
fresh x86_64 and aarch64_generic roots each selected and installed all 62
required packages. The x86 bundle installer then ran on the disposable router
using only its local `packages.adb`, reported RC15 for both packages, restarted
the existing instance and preserved cake-autorate, SQM and network hashes.

Final RC15 release payload hashes are:

| Artifact | SHA-256 |
|---|---|
| x86_64 daemon APK | `9626247b8e2010eded6b77b37e44d62e54c863208004f1d166c567dc20c27346` |
| aarch64_generic daemon APK | `5b9a79a4a33e46251f320be2d767472d55dc0de861096337d347c863af929e92` |
| noarch LuCI APK | `87dd08ad700e16cb4e05a5896a12411c6d7b57d767840e186eec148ce5bba814` |
| x86_64 installer | `66ab3be109de2ebcde1de3c32d7d5ce9e43dbf4762fe91cc1490ec2f9c9a0897` |
| aarch64_generic installer | `ad74f49645626891c5f71f2bea193052f70ad9e7df3bf53bf450878ed4a52511` |
| x86_64 offline bundle | `53e57b7764c7607074043e1819b61d7d6f7eb01c2ae1170b6c95c91e54dcc2bf` |
| rockchip/armv8 offline bundle | `1a750c5dafaba4728de0afe2f95ea638119ccb6c500645feea32a48e57c8d8a1` |

## RC16 native tabs and controller CPU gate (2026-07-15)

RC16 keeps the latency controller on every reflector response but moves work
that does not need probe-rate timing off that hot path. The achieved-rate and
rating detector follows `monitor_achieved_rates_interval_ms`; atomic status
publication is bounded to 4 Hz; healthy SQM checks run every 15 seconds and
return to 3 seconds after a fault; positive CAKE changes are coalesced to 10 Hz
while reductions remain immediate. Multi-WAN member state is still inspected at
the configured route interval, with the validated device/source/mark/table
identity refreshed every 30 seconds or immediately on an error.

A read-only `/usr/libexec/cake-autorate-rs/cpu-profile` helper was used before
and after the upgrade on the same four-core x86_64 Multi-WAN router. It reads
`/proc/stat` and per-process self plus waited-child ticks, uses only a temporary
file in `/tmp`, and reports both one-CPU and total-capacity percentages. The
30-second RC15 baseline attributed about 6.20% of one CPU to the primary
daemon, 3.60% to the standby daemon and 0.80% to the persistent pinger/control
remainder: about 10.6% of one CPU, or 2.65% of four-core capacity. RC16 measured
1.43% and 1.40% for the two daemons, 0.83% and 0.80% for their two pingers, and
0.07% for the scheduler: 4.53% of one CPU, or 1.13% total capacity. That is a
57% reduction for the observed control stack and a 71% reduction for the two
daemons combined. Whole-router busy time was 2.79%, including 0.57% softirq;
that value intentionally includes unrelated forwarding, PPPoE, CAKE and other
router work. A stable 30-second disposable-router sample measured the single
daemon at 0.37% of one CPU (0.18% of its two-core capacity), with router busy at
0.60%.

Authenticated Playwright opened the installed Edit modal on the disposable
router and the production Multi-WAN router. The six groups contained 11, 10,
8, 24, 29 and 20 options, used native `cbi-tabmenu` with exactly one `cbi-tab`
and five `cbi-tab-disabled` items, and retained all inactive form nodes.
Mouse, ArrowRight, Home and End navigation selected the correct ARIA tab. At
390 px the 712-pixel tab strip scrolled inside a 323-pixel modal content area,
the document remained exactly 390 pixels wide, and no links overlapped. Graphs
then rendered one card/two canvases on the disposable router and two stacked
cards/four canvases on the Multi-WAN router without a browser or console error.

The final local gate passed 105 Rust tests, strict Clippy, Rust formatting,
nine shell suites, four LuCI JavaScript suites, shell syntax and
`git diff --check`. Both OpenWrt 25.12.5 SDK builds completed for x86_64 and
rockchip/armv8 (`aarch64_generic`); independently built noarch LuCI APKs have
the same SHA-256. Each regenerated offline repository contains 65 APKs. With
network access and package scripts disabled, fresh x86_64 and
`aarch64_generic` roots selected and installed all 62 required packages. The
x86 bundle installer also ran using only its local `packages.adb` on the
disposable router, reported both RC16 packages, created a dated backup,
restarted the existing instance, and preserved cake-autorate, SQM and network
hashes.

The Multi-WAN router retained `wan_sqm` as `ACTIVE` on `pppoe-wan` and
`wanb_sqm` as `STANDBY` on `eth0`; both managed SQM runtimes remained healthy.
Its cake-autorate, SQM, network and mwan3 hashes were unchanged across the
upgrade:

- cake-autorate: `dc285ba5f5b619ee601c82aef40acb8c6a9fc90cff469f93b34422304d94bc69`;
- SQM: `a328a63cb1252861d070eb7ba53bed97571efdae98445bd518268c674b42a2cb`;
- network: `ed6d568293ed2d632c51827f5fa3227acaec4ed257359eb9a92b85a355065190`;
- mwan3: `fea15a18e8f39f4211ee37959759a5892d0f592dd6e105cf147263b4dde67e81`.

Final RC16 release payload hashes are:

| Artifact | SHA-256 |
|---|---|
| x86_64 daemon APK | `16fa2093ae592551411739b2ca1bcb497ef78e1a01eae99aca4328641a265b80` |
| aarch64_generic daemon APK | `71d27e6d73ca65e21d03bff341bba66a96a5350e16e3fdd1711d77d98d737d0f` |
| noarch LuCI APK | `4cf7b544c51ebfc760fb14ff3832c0c683519d35477ef99c34b559862e660d4a` |
| x86_64 installer | `756950eae6b58332c3e2b8ce5ec42948281e315341a903a97c689cb8372e62cc` |
| aarch64_generic installer | `7a459d1db7c17cbb01381aad3c82e33112dafcec7271146cd3de15d9caa2cb43` |
| x86_64 offline bundle | `98ae66fc3373f7b4fe5301fb3c714458313ce8d71a93d0b69229985db4709cea` |
| rockchip/armv8 offline bundle | `dc901656e376fc66e45e9171b9d06dccdd75877e7eef039e54d8184d8ea26dff` |

## RC17 Full Auto-Tune validation regression and acceptance (2026-07-16)

An anonymized RC16 re-run on a nominal gigabit PPPoE uplink failed closed and
wrote no UCI configuration. The existing SQM queue was restored. That safe
rollback was correct, but the retained evidence showed that the rejection
reason and proposed fixed `0.95` correction were not mathematically sound.

The second shaped candidate was 738.5/755.5 Mbit/s and achieved
683.153/698.955 Mbit/s. RC16 displayed approximately 77.3% for both directions
because it divided achieved throughput by unshaped observed-low capacity. It
did not show that achieved/candidate was about 92.5% in both directions. RC17
therefore has independent regression assertions for:

```text
candidate realization = achieved / candidate
capacity retention    = achieved / observed-low
candidate capacity    = candidate / observed-low
```

The same capture exposed a quantile mismatch. Idle native-transport samples
had median 220 ms and p95 420 ms; loaded p95 was 480 ms. The RC16 calculation
reported `480 - 220 = 260 ms`. Like-for-like comparison gives
`480 - 420 = 60 ms`. An earlier candidate had loaded p95 410 ms and therefore
a clamped p95-to-p95 delta of 0 ms, not the reported 190 ms. The RC17 transport
regression proves persistent native connection reuse and
`max(loaded_p95 - idle_p95, 0)`.

ICMP evidence was also self-contaminated. A roughly 25-second shaped phase
issued 79 rapid `fping -c 1` batches against three addresses from one anycast
provider family. Their reported losses were about 1.3%, 7.6%, and 19.0%, while
idle/loaded p95 RTT was 9.91/10.10 ms (only +0.19 ms). This is consistent with
public-reflector ICMP rate limiting, not proof of WAN loss. RC17 verifies no
more than one loaded batch per second, at least three provider/address
families, and median per-reflector loss.

Router CPU peaked at 53.2%, below the hard gate. Route identity, external path,
and pinned speed-test server remained stable. However, RC16 had only an initial
aggregate quiet check, so absence of forwarded client traffic during every
heavy phase could not be proven. The correction adds temporary per-phase
nftables forwarding counters. Router-originated speed-test bytes are excluded;
missing counters or more than max(2% of the directional reference, 1 Mbit/s)
mark the phase contaminated. Each phase gets one repeat before strict failure.

The completed deterministic RC17 gate covers:

1. Replaying the numbers above returns candidate realization 92.5%, capacity
   retention 77.3%, and an `infeasible` result when the required retention
   floor leaves no legal downward correction.
2. Candidate realization is a two-sided 80..110% gate. A low or implausibly
   high result requests the same measurement again; bounded retry exhaustion
   is `INCONCLUSIVE`, not a proved candidate failure.
3. Clean low retention raises only the failed direction within observed-low,
   configured maximum, and per-revision bounds.
4. Adverse latency/loss/CPU can reduce a direction only when the predicted
   retention remains at or above the safety floor.
5. Persistent WebSocket/HTTPS transport uses every valid raw sample for
   p95-to-p95 deltas; an exact `[10, 10, 10, 200]` tail cannot be erased by a
   robust central filter. TCP-connect is rejected before any heavy phase.
6. ICMP is rate-limited and family-diverse. A reflector set which becomes
   rate-limited under load makes the run inconclusive and is never silently
   re-baselined. Phase-background evidence is present in success and error
   diagnostics.
7. Every shaped helper call is bracketed by exact ownership/rate checks for the
   temporary root CAKE qdiscs, IFB, and ingress redirect. Plausible helper JSON
   cannot pass after either the precondition or postcondition is invalidated.
8. A failed, incomplete, contaminated, conservative, infeasible, or
   inconclusive result has no LuCI apply action and cannot pass scheduled
   Auto-Apply. The scheduler requires the exact current 14-gate set.
9. Cancellation/error atomically leaves job status terminal and restores the
   selected qdisc/SQM state without touching the other WAN.
10. Apply tests exercise both packages as one guarded rollback transaction:
    pre-existing guard-section collisions, exact list ordering, route drift,
    old-plus-new daemon overlap, duplicate/child root qdiscs, confirm retry,
    no-data reconciliation, timeout rollback, tmpfs-token loss after marker
    removal, and fail-closed indeterminate state.

All local acceptance gates passed:

- `cargo fmt --check`, `cargo check --all-targets`, and 136 Rust unit tests;
- all six daemon shell suites, all eight LuCI shell suites, and all five LuCI
  JavaScript suites;
- `git diff --check` and POSIX-shell syntax checks for every changed runtime
  helper;
- independent OpenWrt 25.12.5 x86_64 and rockchip/armv8 builds. Their noarch
  LuCI APKs are byte-identical;
- fresh `--no-network --no-scripts` installation of all 68 indexed packages
  from each offline repository;
- authenticated desktop and mobile Playwright flows for Status, Graphs,
  Settings, Edit, and Re-run Auto-Tune on the production x86 Multi-WAN and ARM
  routers. The final graph test also covers transitive clustering of dense
  `OFFLINE`/`LEARNING`/`ACTIVE` event chains.

The production upgrade replaced only the same-version LuCI package. On the
x86 Multi-WAN router, both daemon instances remained running and the existing
PPPoE/secondary-WAN CAKE qdiscs remained at 794400/14500 kbit/s. The
`cake-autorate`, SQM, network, and mwan3 configuration hashes remained
unchanged. On the ARM router, the instance remained running, its CAKE qdisc
remained at 15773 kbit/s, and the `cake-autorate`, SQM, and network hashes also
remained unchanged. An existing schema-2 diagnostic is now exposed as
read-only `state=legacy`, wrapped in schema 3 with producer
`cake-autorate-rs-autotune`, `auto_apply_eligible=false`, and
`configuration_written=false`; it cannot enter Review or scheduled Auto-Apply.

Final RC17 release payload hashes are:

| Artifact | SHA-256 |
|---|---|
| x86_64 daemon APK | `ac5cf3ab58ebb8b3e4857a9ad4aac5639460bcab70bfcae6c281ea126b8ed650` |
| aarch64_generic daemon APK | `85bb5d288f198299f1be6845d983a723cf5194f214766bc8c1fe14812e2e3eae` |
| noarch LuCI APK | `b689258836d7ad1e3b85e7ba8b1d9b99c559abeac782e5ee9d02d028009999cd` |
| x86_64 installer | `7719a8d40b27746f6ef9163c61e38af96659f8fc5fc5324c2a7825312d4899f0` |
| aarch64_generic installer | `d177d34effc2421fbc6fa16e6ff8ec37981a48b564bbc3c2db69fb45f84ae5f4` |
| x86_64 offline bundle | `c60babaa865106ce0d739b4bad599c6b1faf038970c30d3c740314ac3c380ea6` |
| rockchip/armv8 offline bundle | `5c8ce4c0beb649e9534ecb0f02af066140678b5e0a76f3e2bbc273ed6fdd7168` |

## RC18 profile-aware Full Auto-Tune acceptance (2026-07-16)

RC18 adds Gaming, Best overall and Fair as complete calibration contracts.
The selected profile is carried through the worker identity, proposal schema
2, result schema 4, status/cancel/attestation calls, temporary qdisc policy,
guarded UCI/SQM apply, scheduler eligibility and rollback. Unknown or
mismatched profile data fails closed. Existing RC17 callers and instances
without a saved profile resolve to Best overall; the historical `balanced`
CLI value is a read-compatible alias.

The deterministic gate passed:

- `cargo fmt --check`, `cargo check --all-targets`,
  `cargo clippy --all-targets -- -D warnings`, and 140 Rust unit tests;
- all six daemon shell suites, all eight LuCI shell suites, and all five LuCI
  JavaScript suites;
- syntax checks for every runtime shell helper and JavaScript view,
  `git diff --check`, and both OpenWrt 25.12.5 SDK builds;
- profile matrices for stable and variable links, profile parsing/aliases,
  target grades, exact validation gates, adaptive-ceiling cadence and Gaming
  `diffserv4` output;
- fail-closed result/profile/SQM mismatches in the lifecycle, scheduler,
  recovery and apply guard;
- exact temporary qdisc verification including class mode, direction-specific
  `wash`, `nat`, bandwidth, PPPoE/Ethernet overhead, IFB and ingress redirect.

The actual Full Auto-Tune lifecycle was then run for all three profiles on a
disposable x86 OpenWrt router. During Gaming validation, `tc` showed
`diffserv4 triple-isolate nat nowash` on both the selected Ethernet root and
the temporary IFB. Best overall and Fair showed best-effort upload without
`wash` and best-effort IFB download with `wash`, matching
`piece_of_cake.qos`. The small two-vCPU virtual router could not establish a
trustworthy passing shaped result: candidate realization and/or CPU evidence
remained outside the selected contract. Each run therefore ended
`INCONCLUSIVE`, exposed no apply action, wrote no UCI configuration, restored
the original 10/85 Mbit/s CAKE pair, removed its temporary recovery state and
reported `runtime_restored=true`. This is the expected safe outcome rather
than a calibration success claim. One initial Best overall reflector-planner
attempt also failed transiently and a bounded retry completed normally.

The disposable router's configuration SHA-256 values were identical before
and after all profile runs and the final offline-installer test:

| Config | SHA-256 |
|---|---|
| `cake-autorate` | `aaf00467c59f1c3f573925791cfbca71382f6cf86125bee2328ac67d0116b3bb` |
| `sqm` | `0204b58ff12277f15aa536e1406ee0dbf2aeeb739f7b48c7169a2b598ecb8d68` |
| `network` | `ea57aa4b5e44ca7b02c3ea84c174688f9f0185200077f3e87a39f1a071a280ce` |

The final packages were installed on both production targets without running
heavy calibration. On the x86 nftables-mwan3 router, both autorate instances
and all four mwan3 tracker/route-monitor processes remained running; mwan3 was
never restarted. Existing PPPoE and backup-WAN CAKE/IFB queues remained
present, including PPPoE overhead 44/MPU 84. All four configuration hashes
were unchanged:

| Config | SHA-256 |
|---|---|
| `cake-autorate` | `88cc2cd79dffa695fa8b16b96a5fc375d31a95adac6e724773a5373b1c2dd6d8` |
| `sqm` | `a4405847b3044c9f41c09097fd9e850915d7113953502ec60ccb8365b93eb8bb` |
| `network` | `ed6d568293ed2d632c51827f5fa3227acaec4ed257359eb9a92b85a355065190` |
| `mwan3` | `fea15a18e8f39f4211ee37959759a5892d0f592dd6e105cf147263b4dde67e81` |

On the rockchip/armv8 router, the existing WWAN instance remained running and
continued to manage its dynamic CAKE pair. Its configuration hashes were also
unchanged:

| Config | SHA-256 |
|---|---|
| `cake-autorate` | `ac59a8a2a26e88803a5c493ea86c840c3dd9c10a2058ce0768164515abcdb10c` |
| `sqm` | `1e29f86a4cfba8cecaa5aee5cface48c6ef2599fa9aa7a567c4d363ec641c920` |
| `network` | `3d16217f0e3ec73b9ba55b006caf30c5abda024126c429df30ca93e170e4ea68` |

Authenticated Playwright opened the installed Settings and Re-run Auto-Tune
wizard at 1500×900 and 390×844 on the disposable x86, production x86
Multi-WAN and production ARM routers. It verified that Best overall is the
default for an old instance, selected Gaming/Best overall/Fair, checked target
and policy text, and recorded no page or console errors. A final visual pass
confirmed that profile cards wrap only at word boundaries on both LuCI themes.
No calibration was started and no settings were saved by the browser test.

Both offline repositories index 68 APKs. Fresh network- and script-disabled
x86_64 and aarch64_generic roots selected and installed all 68 packages,
including the default mbedTLS provider. Both installers pass `sh -n`; both
archives contain exactly one platform installer, `packages.adb`, and 68 APKs.
Running the exact x86 archive installer on the disposable router first proved
its 8 MiB rootfs safety gate by refusing after an intentionally too-tight
`/root` extraction, before changing packages. Running the same unmodified
bundle from `/tmp` then completed, made a dated UCI backup, restarted only
CAKE Autorate and preserved the three configuration hashes above.

Final RC18 payload hashes are:

| Artifact | SHA-256 |
|---|---|
| x86_64 daemon APK | `aae1935a57ae350624eeaf6911cb67c981aeba5283d92c8c413b243fd25da032` |
| aarch64_generic daemon APK | `982e87b9072c4c8ed2ee9fb23f657e6bfd8d9c808a9a5ced5682566f424aa99a` |
| noarch LuCI APK | `e7830b65c3af94dd5c972c42c9749d8d81bff64198142c38aa46c567de3c001d` |
| x86_64 installer | `39f0db3d56c646419f700c8fffdc86ae2efe4a5764684682d055d5bd5eef318f` |
| aarch64_generic installer | `e34b9dee545826b79db3e8f1f4379d1db410585cc0da4f6d4fa53926f6a34321` |
| x86_64 offline bundle | `afc9aad043bc5cea3c998e50a80f7e5bd40857d112beb01bf57fb8b207bb92f6` |
| rockchip/armv8 offline bundle | `386ef81777e4784912183ca0fd73611c88e758f76d7c82444ca318f718983cfb` |

## RC19 throughput-first Fair and supervisor acceptance (2026-07-16)

RC19 changes Fair from a hard B/60 ms contract to a throughput-first
class-C/200 ms goal above a hard 90% observed-low capacity floor. Proposal
schema 3 records `quality_target_required=false` and
`throughput_priority=true`; result schema 5 separates `hard_pass` from
`quality_target_met`, records the actual grade and binds the terminal result to
an immutable run ID, configuration fingerprint, phase evidence and restored
runtime state.

The deterministic Fair gate covers three Review outcomes:

- a class-C-or-better candidate remains eligible for normal validated apply;
- a complete hard-safe candidate which misses only the quality goal can be
  applied manually or discarded with **Keep current settings**;
- an existing managed instance may receive a separate disable-SQM comparison
  suggestion only after a simultaneous bidirectional unshaped control proves
  SQM pause/bypass, no temporary shaper, clean forwarded-background counters,
  no worse grade, no more than 10 ms worse effective delay, and at least 2%
  throughput gain in both directions.

The disable choice is deliberately not preselected and can never be scheduled.
Negative LuCI and Apply Guard tests reject one-direction evidence, missing
pause/bypass proof, contaminated or unavailable counters, either directional
gain below 2%, worse latency/grade, mismatched action/run/fingerprint and any
post-apply daemon, CAKE, IFB, clsact or ingress-redirect residue.

The speed-test supervisor gate additionally verifies:

- the helper starts stopped and cannot run before its process identity and
  recovery journal entry are established;
- the complete isolated group receives bounded TERM then KILL on timeout or
  cancel, including a child which ignores TERM;
- valid JSON followed by a non-zero helper exit remains a failed raw diagnostic
  and is never promoted;
- only one bounded root-owned JSON object from an exact zero exit is published
  atomically;
- crash recovery restores runtime/SQM state and leaves no orphan helper;
- per-instance terminal history remains in RAM and is pruned by both run count
  and byte limit.

The complete local gate passed:

- `cargo fmt --check`, `cargo check --locked --all-targets`,
  `cargo clippy --locked --all-targets -- -D warnings`, and all 142 Rust tests;
- all daemon and LuCI shell suites, all LuCI JavaScript suites, changed-helper
  `sh -n`, changed-view `node --check`, and `git diff --check`;
- OpenWrt 25.12.5 x86_64 and rockchip/armv8 SDK builds. The two SDKs produced
  byte-identical noarch LuCI packages.

Both final offline repositories index 68 APKs. Fresh architecture-specific
roots with networking and package scripts disabled selected and installed all
68 packages. Both installers pass `sh -n`, both manifests verify, and each
archive contains one platform installer, `packages.adb`, and 68 APKs.

The exact x86_64 archive was then installed on the disposable router. It made
backup `rc19-install-20260716-173514`, left its existing instance running with
the original CAKE/IFB topology, and preserved all four configuration hashes:

| Disposable x86 configuration | SHA-256 |
|---|---|
| `cake-autorate` | `aaf00467c59f1c3f573925791cfbca71382f6cf86125bee2328ac67d0116b3bb` |
| `sqm` | `0204b58ff12277f15aa536e1406ee0dbf2aeeb739f7b48c7169a2b598ecb8d68` |
| `network` | `ea57aa4b5e44ca7b02c3ea84c174688f9f0185200077f3e87a39f1a071a280ce` |
| `mwan3` | `88c720fe486115b5a3db09b6efd3b7519878c35105a7ad2a86b0e8127c8f6b96` |

The same exact release archives were installed on the two production
acceptance routers:

- the x86_64 Multi-WAN router made backup
  `rc19-install-20260716-203724`; `wan_sqm` and `wanb_sqm` remained running,
  both WAN members and their trackers remained online, mwan3 was not
  restarted, and the existing PPPoE and Ethernet CAKE/IFB pairs remained
  present;
- the aarch64_generic router had only 7.4 MiB free in its root filesystem, so
  its old `/root/packages` cache was moved temporarily to `/tmp` under an
  exit/signal restoration trap. The normal installer safety check then passed,
  backup `rc19-install-20260716-203922` was created, the cache was restored,
  and `wwan_adaptive` plus its CAKE/IFB pair remained running.

Post-install hashes exactly matched the pre-install values:

| Router/configuration | SHA-256 |
|---|---|
| x86 Multi-WAN `cake-autorate` | `88cc2cd79dffa695fa8b16b96a5fc375d31a95adac6e724773a5373b1c2dd6d8` |
| x86 Multi-WAN `sqm` | `a4405847b3044c9f41c09097fd9e850915d7113953502ec60ccb8365b93eb8bb` |
| x86 Multi-WAN `network` | `ed6d568293ed2d632c51827f5fa3227acaec4ed257359eb9a92b85a355065190` |
| x86 Multi-WAN `mwan3` | `fea15a18e8f39f4211ee37959759a5892d0f592dd6e105cf147263b4dde67e81` |
| ARM `cake-autorate` | `ac59a8a2a26e88803a5c493ea86c840c3dd9c10a2058ce0768164515abcdb10c` |
| ARM `sqm` | `1e29f86a4cfba8cecaa5aee5cface48c6ef2599fa9aa7a567c4d363ec641c920` |
| ARM `network` | `3d16217f0e3ec73b9ba55b006caf30c5abda024126c429df30ca93e170e4ea68` |

Authenticated Playwright opened Status, Graphs, Settings and Re-run Auto-Tune
at 1500x900 and 390x844 on all three routers. It selected and checked Gaming,
Best overall and Fair, including Fair's throughput-first/class-C/90% contract
and possible evidence-backed disable-SQM choice. The virtual router rendered
two canvases, the production Multi-WAN router four, and the production ARM
router two. Every run reported zero browser/page errors and
`scrollWidth == clientWidth`; a visual pass also confirmed readable desktop
tables, mobile cards, vertically stacked WAN graph cards, and word-safe profile
text. The browser tests did not start calibration or save configuration.

Final RC19 payload hashes are:

| Artifact | SHA-256 |
|---|---|
| x86_64 daemon APK | `621e4375e2a4a460a3b9351c6f6878d9d07f5cbfb30b169e0ce85ce73f8b8f12` |
| aarch64_generic daemon APK | `51b74c5e67c6e65952f3e723a257d09d2c918afef5d894f0bfdc425130b97a52` |
| noarch LuCI APK | `441b2eb34e9d8a452e24cb053bad971fb33de9e477441a5af3ac83587d8c3f63` |
| x86_64 installer | `f9b46243ed94d2e0b9c5351d51c6d4cbf1a83adbb035229893961633555f047e` |
| aarch64_generic installer | `aef2f47e28b9f3321789f6e5b854a8ef17c53917c2ca45a6ae09e40d1011b6a0` |
| x86_64 offline bundle | `e9a2b17c9cc2d513cb0e2c7862cbf4ed8fab468f48bfab04d716452583689b7a` |
| rockchip/armv8 offline bundle | `93f8c3a95ba847a4f3b4aab12e67d92eed238939168469bd611f6a081bbd7d64` |

## RC20 runtime ownership and traffic-priority acceptance (2026-07-17)

RC20 makes the actual data-plane state observable and keeps ownership
unambiguous. The mandatory **Services** status column reports the daemon,
managed SQM section, upload/download CAKE qdiscs, IFB, ingress redirect,
Apply Guard/operation state and the optional classifier independently. It can
therefore distinguish a disabled service from an orphan shaper instead of
inferring health from a stale status file.

The traffic-priority feature borrows the useful profile/rule editing model
from packet-classification frontends, but deliberately has no qosify or eBPF
integration. It adds no second SQM service and never creates, changes or
deletes a qdisc, IFB or bandwidth rate. The helper owns only
`table inet cake_autorate_dscp`; CAKE Autorate remains the sole owner of SQM,
CAKE and rates. Upgraded instances are opt-in. Rules are validated as
structured protocol/port/address fields, rendered without a shell command and
applied only when the selected upload CAKE queue is `layer_cake.qos` with
`diffserv4`.

The deterministic local gate passed:

- `cargo fmt --check`, locked `cargo check`, Clippy with `-D warnings`, and all
  142 Rust tests;
- all seven daemon shell suites, all nine LuCI shell suites and all seven LuCI
  JavaScript suites;
- POSIX syntax checks for every packaged runtime helper, JavaScript syntax
  checks for every LuCI view and `git diff --check`;
- boot-aware Apply Guard recovery, server-side confirmation supervision,
  immutable receipts, stale-marker cleanup and exact rollback tests;
- structured speed-test failure propagation, package-upgrade LuCI cache
  invalidation, mandatory runtime-health rendering and profile/rule validation
  tests;
- independent OpenWrt 25.12.5 x86_64 and rockchip/armv8 SDK builds. The two
  noarch LuCI APKs are byte-identical.

The native classifier was exercised on the disposable x86 router rather than
only through mocks:

1. A temporary Gaming profile produced upload and IFB download CAKE queues at
   the original 10/85 Mbit/s rates, both using `diffserv4`.
2. The helper loaded its isolated forward/output nftables chains and an
   ordered WireGuard custom rule mapping UDP/51820 to AF41.
3. An out-of-band rule mutation changed status to `DRIFTED`; reapply restored
   an attested `ACTIVE` table.
4. Stopping CAKE Autorate removed the private table and the managed CAKE/IFB
   runtime; starting it restored the configured runtime.
5. The exact pre-test files were restored. Final hashes remained:

| Disposable x86 configuration | SHA-256 |
|---|---|
| `cake-autorate` | `aaf00467c59f1c3f573925791cfbca71382f6cf86125bee2328ac67d0116b3bb` |
| `sqm` | `0204b58ff12277f15aa536e1406ee0dbf2aeeb739f7b48c7169a2b598ecb8d68` |
| `network` | `ea57aa4b5e44ca7b02c3ea84c174688f9f0185200077f3e87a39f1a071a280ce` |
| `mwan3` | `88c720fe486115b5a3db09b6efd3b7519878c35105a7ad2a86b0e8127c8f6b96` |

Both offline repositories index 68 APKs. Fresh network-disabled,
script-disabled x86_64 and aarch64_generic roots selected the complete
68-package closure including the fallback mbedTLS provider. Both installers
pass `sh -n`; each archive contains one platform installer, `packages.adb` and
68 APKs. The exact x86 archive installed on the disposable router, created
backup `rc20-install-20260716-214352`, restarted only CAKE Autorate, reported
the active instance HEALTHY and preserved all four hashes above.

The exact release archives were then installed on two existing acceptance
routers without running a heavy calibration:

- On the ARM router, the existing package cache was moved from the nearly full
  overlay to tmpfs under an EXIT/HUP/INT/TERM restoration trap. This allowed
  the unchanged 8 MiB installer safety gate to pass. Backup
  `rc20-install-20260717-004718` was created, the cache was restored, and
  `wwan_adaptive` remained HEALTHY with one daemon, the managed queue, both
  CAKE qdiscs, IFB and ingress redirect active. All configuration hashes were
  unchanged:

| ARM configuration | SHA-256 |
|---|---|
| `cake-autorate` | `ac59a8a2a26e88803a5c493ea86c840c3dd9c10a2058ce0768164515abcdb10c` |
| `sqm` | `1e29f86a4cfba8cecaa5aee5cface48c6ef2599fa9aa7a567c4d363ec641c920` |
| `network` | `3d16217f0e3ec73b9ba55b006caf30c5abda024126c429df30ca93e170e4ea68` |

- On the x86 Multi-WAN router, backup
  `rc20-install-20260717-004910` was created. The four tracker PIDs and two
  route-monitor PIDs were identical before and after installation; mwan3 was
  never restarted. `network` and `mwan3` hashes were unchanged. Startup
  removed one expired Apply Guard marker and synchronized the managed primary
  SQM queue from its authoritative instance values (894500/889200 kbit/s);
  those are the only configuration differences. The resulting hashes remained
  stable through the browser audit:

| x86 Multi-WAN configuration | Before | Final |
|---|---|---|
| `cake-autorate` | `64fd14786b8d83d8915d7df05853053b98d0812a1220177f9aaef38b96c3fd40` | `bc9061adcfab4bff099146fdf0492494e6394b39bc00183da9c0a73f2e8f9550` |
| `sqm` | `dd46843564cb4612be739ca40dacfbd3e9fb4223355cefb89aa269c4a719e484` | `08a7ba40701ec94145a147876f20a6258b5276540261a439ddffb3b5a1896e59` |
| `network` | `ed6d568293ed2d632c51827f5fa3227acaec4ed257359eb9a92b85a355065190` | unchanged |
| `mwan3` | `fea15a18e8f39f4211ee37959759a5892d0f592dd6e105cf147263b4dde67e81` | unchanged |

Both `wan_sqm` and `wanb_sqm` reported HEALTHY with independent daemon,
queue, CAKE and IFB state. Member-scoped HTTP and ICMP probes proved distinct
paths: the primary used PPPoE/table 1 and the backup used Ethernet/table 2;
the observed external addresses differed and both members had zero packet
loss. This verifies route binding without publishing customer addresses.

Authenticated Playwright checked Status, Graphs, Traffic priorities, Settings,
Edit and Re-run Auto-Tune at 1500x900 and 390x844 on the disposable x86,
production x86 Multi-WAN and production ARM routers. It rendered 2/4/2
canvases respectively, found zero page/console/RPC errors and zero horizontal
overflow on every page. A visual pass confirmed mandatory Services details,
stacked Multi-WAN graph cards, fixed chart labels, readable mobile modals and
the new profile-rule editor. The browser did not start calibration or save
configuration. Evidence is retained under:

- `/home/w0w/cake-autorate-rs-owrt/test-logs/rc20-playwright-virtual`;
- `/home/w0w/cake-autorate-rs-owrt/test-logs/rc20-playwright-77`;
- `/home/w0w/cake-autorate-rs-owrt/test-logs/rc20-playwright-100`.

Final RC20 payload hashes are:

| Artifact | SHA-256 |
|---|---|
| x86_64 daemon APK | `76a84fb7a8b1bc02af61354ede586de3d25e2e0fc82179fd567744bd51ebacee` |
| aarch64_generic daemon APK | `30d6f27b2522b6c6d6ece1cdba7a78859b4b4092ba7e51096960e95b360b7686` |
| noarch LuCI APK | `c48f8cfdae1d08304398750489473ce43a9154fb2b254d6a36643fe504ff69b6` |
| x86_64 installer | `59c3901caf8130d1df73eadc6d6b3ed8eaf2e18c7dcf7e85530268b742fdfe8c` |
| aarch64_generic installer | `f942ca9bea8180cf0dd73a97f37159be1d4fc5d531e0e5af09730972fa8a0231` |
| x86_64 offline bundle | `1c8d02d98d7d8cc295534ba50ddb5acc4ff83085188e10c5d9fb550fa6d03bd7` |
| rockchip/armv8 offline bundle | `bd1833db70edf9cc5d8406979e30a22e2bdcaae7c967e8fdc297e8d94a3ac4d5` |

## RC21 instance-scoped priorities and upgrade-lifecycle acceptance (2026-07-17)

RC21 was tested against the failure modes observed on minimal OpenWrt images,
not only against a development host. The affected x86 Multi-WAN image has no
`od` or `cksum`; its kernel UUID source is present. The new preflight consumed
that source directly, produced an exact 32-hex worker token and unique
temporary IFB identity, entered the normal reflector phase, and cancelled with
`runtime_restored=true` and no pending recovery. Deterministic fixtures also
proved that an invalid/missing UUID or eight colliding foreign interface names
fail synchronously before a worker, runtime lock, SQM pause, or recovery
journal is created. A foreign colliding interface is never deleted.

The final local release gate passed:

- `cargo fmt --check`, locked `cargo check`, strict Clippy and all 142 Rust
  tests;
- all seven daemon shell suites, all nine LuCI shell lifecycle/recovery suites
  and all seven LuCI JavaScript suites;
- POSIX syntax for every packaged helper, JavaScript syntax for every LuCI
  view/test, JSON parsing for menu/ACL data, and `git diff --check`;
- independent OpenWrt 25.12.5 x86_64 and rockchip/armv8 builds. Their noarch
  LuCI APKs are byte-identical, and both core APKs contain the expected ELF
  architecture plus matching `post-install` and `post-upgrade` restart hooks.

Package replacement was then verified on a disposable x86 router, an existing
dual-WAN x86 router, and an existing low-storage ARM router. In all three
cases the running daemon PID changed automatically during `post-upgrade`; no
manual restart or reboot was needed. Every pre/post UCI checksum remained
identical. The dual-WAN device retained both managed CAKE/IFB topologies and
limits, both instances reported `HEALTHY`, and every IPv4/IPv6 mwan3 member
remained online. On the ARM device the package cache was moved to RAM only
under an EXIT/HUP/INT/TERM restoration trap; its post-test file manifest was
byte-identical to the backup, rootfs free space returned to its initial value,
and the managed instance remained `HEALTHY`. No device retained a runtime
recovery file.

The LuCI navigation was tested both from a fresh session and through an actual
RC20-to-RC21 browser-cache transition. In the latter case the same browser
session first rendered the old global Traffic priorities tab, received RC21,
and used only an ordinary reload. RC21 removed that stale tab, flushed LuCI's
menu cache, kept the login session, rendered one Traffic priorities action per
instance, and opened a view containing only the selected instance and its
rules.

Authenticated Playwright then audited the exact final packages on the
disposable x86, dual-WAN x86, and ARM routers at 1500x900 and 390x844. It
covered Status, Graphs, Settings, instance-scoped Traffic priorities, Re-run
Auto-Tune and Edit. The routers rendered 2, 4, and 2 graph canvases
respectively. Every page had zero horizontal overflow, zero page/console/RPC
errors, and no `Access denied` response. The checks did not start a heavy
calibration or save configuration.

Both offline repositories contain and index 68 APKs. The standalone installers
pass `sh -n`; each archive contains the matching installer, `packages.adb`, and
the same 68-package closure. Fresh x86_64 and aarch64_generic roots, with
network and package scripts disabled, installed all 68 packages and selected
exactly daemon/LuCI `1.0_rc21-r1`.

Final RC21 payload hashes are:

| Artifact | SHA-256 |
|---|---|
| x86_64 daemon APK | `56e840c2ad202f109b76450c4789cd6c2da9076701b2b8c4b52c0b2616228a74` |
| aarch64_generic daemon APK | `099240b5d0dc924f01e883f49755726ca0828f7cc3f75a26b90d00cf7ca8cb2f` |
| noarch LuCI APK | `b65ce190acdf6fb01713f05850d4c3cb6ae015e6ebc3ad4313d3b9b41b87c01c` |
| x86_64 installer | `5d4d8a545ea3a1e2eace825a1e6f2ace34c32865a85c43cfe04e0f2ad0e287d0` |
| aarch64_generic installer | `25939171a0541677dd97941d4a175769292de160cfc0910be8b6067fde7dc013` |
| x86_64 offline bundle | `cd11384235a90d0ec4c380b7b75a759f235a2f50e55f634ec04b43f2cf557940` |
| rockchip/armv8 offline bundle | `7e957b41ba893023168d52462208e398681f466470b40b8e21915755b6dac807` |

## RC22 Pareto Auto-Tune and capacity-floor acceptance (2026-07-17)

RC22 replaces the single pass/fail Auto-Tune candidate with a bounded,
profile-aware search over measured throughput and loaded latency. Gaming
selects the highest-throughput A+ point when A+ is attainable, then falls back
to the best attainable grade and the fastest point within that grade. Best
overall applies the same rule around A. Fair selects the fastest safe point;
when candidates are within 1.5% of the best throughput it prefers the lower
loaded delay. The immutable measured-capacity floors remain 70%, 80%, and 90%
for Gaming, Best overall, and Fair respectively.

When the shaped result does not realize its requested candidate, Auto-Tune may
perform up to eight bounded observations. A shaped-path ceiling is accepted as
repeatable only after a pair of same-candidate results agrees within 5%. RC25
also covers volatile shared-medium links: after three mutually inconsistent
but clean samples, the worst achieved value seeds a lower candidate. That rate
must pass the hard realization interval before selection. Falling below the
common 50% historical-throughput boundary then adds a warning and prevents
Auto-Apply; quality/integrity failures remain inconclusive. No profile lowers
its objective to manufacture an automatic pass.
If a repeatable implementation ceiling makes the safety floor impossible,
Review reports `capacity-floor-infeasible`, keeps the current
configuration selected, and exposes measured diagnostic limits without an
Apply action. Fair may additionally offer an explicit disable-SQM experiment
only when its separate no-SQM control proves material throughput recovery and
no material loaded-latency benefit from SQM.

The final local release gate passed:

- `cargo fmt --check`, locked `cargo check`, strict Clippy, and all 153 Rust
  tests;
- all seven daemon shell suites, all nine LuCI shell lifecycle/recovery suites,
  and all seven LuCI JavaScript suites;
- POSIX syntax for every packaged helper, JavaScript syntax for every LuCI
  view/test, JSON parsing for menu/ACL data, and `git diff --check`;
- regression coverage for Pareto selection, all three immutable floors,
  repeated low/high realization, inconclusive noisy observations, compute and
  shaper ceilings, strict Review action binding, and fail-closed defaults;
- independent OpenWrt 25.12.5 x86_64 and rockchip/armv8 builds. The two noarch
  LuCI APKs are byte-identical.

On the disposable x86 router, a real Fair calibration reached the new
capacity-floor-infeasible branch after five shaped observations. The repeated
CAKE/CPU ceiling was reported explicitly, the measured quality remained A,
and no UCI file or qdisc rate changed. A deterministic browser fixture then
verified that the native desktop and native 390-pixel mobile layouts show no
Apply choice, select Keep current by default, keep Disable SQM explicit, and
have zero horizontal overflow or browser/RPC errors. Evidence is retained in:

- `/home/w0w/cake-autorate-rs-owrt/test-logs/rc22-playwright-disposable-final`;
- `/home/w0w/cake-autorate-rs-owrt/test-logs/rc22-playwright-fair-real`.

The final packages were installed on the production dual-WAN x86 and ARM
acceptance routers using local APKs without restarting network or mwan3. Their
daemon PIDs changed through the package lifecycle hook; all pre/post UCI hashes
and managed qdisc limits remained identical. All four IPv4/IPv6 mwan3 members
remained online on the dual-WAN router. Authenticated Playwright covered
Status, Graphs, Settings, Traffic priorities, Edit, and Re-run Auto-Tune at
1500x900 and 390x844. It rendered 4 and 2 graph canvases respectively, with
zero horizontal overflow and zero page/console/RPC errors. Evidence is retained
under:

- `/home/w0w/cake-autorate-rs-owrt/test-logs/rc22-playwright-77-final`;
- `/home/w0w/cake-autorate-rs-owrt/test-logs/rc22-playwright-100-final`.

Both offline repositories contain and index 68 APKs. Fresh x86_64 and
aarch64_generic roots, with networking and package scripts disabled, installed
the complete 68-package closure and selected exactly daemon/LuCI
`1.0_rc22-r1`. Independently extracted bundles also contain 68 APKs each; their
installer and project APKs are byte-identical to the standalone release
assets. `SHA256SUMS` validates all seven payloads.

Final RC22 payload hashes are:

| Artifact | SHA-256 |
|---|---|
| x86_64 daemon APK | `102b501664e8635ab5fc68ffb313d5b074d56f6bee1ec4d1e1f93e29a9c41c8a` |
| aarch64_generic daemon APK | `72f284473e8baf4db05680675b1b21754184b0af3e255677796e0c48f4484730` |
| noarch LuCI APK | `6d7110a33f0042e2a0c5e7fae27468b8656cf60e04ae98e309c5e574b8fe5eea` |
| x86_64 installer | `afc37bcdbee96cd8f73d8ff2353eeb48ca8cad5ad8df46fb403d76d4611cbdf9` |
| aarch64_generic installer | `1eca9340c049feddc04f65d9a9a095522df3d4a4ff1ffb8f52e13f184c2fc92d` |
| x86_64 offline bundle | `70a39ca945f858f3b4454722523501ceff8697392d8ceca027a6698f9d469fb2` |
| rockchip/armv8 offline bundle | `004f731774a3b26246e5cf75425dd77bccf4cbc6b35f3ce58e24dbf1d68d2ac9` |

## RC23 sustained-CPU and packet-steering diagnosis (2026-07-17)

RC23 was triggered by a Fair download observation whose candidate realization
was credible while the busiest CPU/softirq path remained saturated. RC22
repeated only low-realization candidates; this reliable-but-CPU-unsafe branch
could therefore exhaust its attempt budget with no safe selected point and
return an unhelpful fallback. The corrected optimizer independently handles
each direction, measures the observed-low upper bound, requires repeatable
same-rate CPU evidence, and probes the calculated immutable-floor candidate
before declaring a compute ceiling. A non-CPU resource failure repeats and
then becomes inconclusive rather than an applicable fallback.

The deterministic gate covered the original Fair sequence, continued upload
search at a boundary candidate, the hard-floor probe, a repeatable compute
ceiling with a non-null diagnostic selection, and repeated loss/resource
failure. All 156 Rust tests passed. The complete Auto-Tune shell lifecycle and
LuCI JavaScript tests also passed, including a forced detailed-terminal write
failure: the worker retained the original error and stage in a compact
schema-valid RAM terminal instead of publishing a generic interrupted result.

### Anonymous four-core PPPoE A/B

An existing OpenWrt 25.12.5 x86_64 Multi-WAN router was used for a bounded,
runtime-only diagnosis. Its primary physical ingress exposed four RX queues,
all with the same single-CPU RPS mask. OpenWrt Packet Steering was enabled, but
the effective masks were `8,8,8,8`. No UCI, network, mwan3, IRQ, XPS or qdisc
setting was changed for the control run.

The unshaped control was approximately 0.9/0.9 Gbit/s and class A+. During
shaped download, application throughput was about 0.82 Gbit/s while effective
CPU/softirq repeatedly reached 100%; one observation contained nine
consecutive samples above the 85% profile limit. Upload remained materially
below that compute ceiling. RC23 returned the explicit manual-only
`repeatable-compute-ceiling-below-capacity-floor` result instead of a null or
applicable fallback.

For the comparison, only the four volatile `rps_cpus` files were changed from
`8` to `f`, distributing software RX work across all four CPUs on this specific
router. Download CPU peaks became variable rather than continuously saturated,
and the bounded search progressed to an approximately 0.884 Gbit/s selected
download candidate; upload independently progressed to approximately
0.890 Gbit/s. A later PPPoE address loss invalidated the run before terminal
confirmation, as required. It is therefore evidence of a datapath bottleneck,
not a completed calibration result or a universal recommendation.

The original `8` masks were restored immediately. Exact pre-test
cake-autorate, SQM, network and mwan3 hashes still matched; both daemon
instances were running; and the pre-existing primary and backup CAKE/IFB rates
were restored. The project never applies the all-CPU mask automatically.
Operators evaluating OpenWrt's **Enabled (all CPUs)** mode must repeat their own
bounded A/B because hardware RSS, IRQ affinity, NAPI placement, XPS, cache/IPI
costs, PPPoE/IFB work and qdisc locking differ by platform. The
`steering_flows` field applies to local-socket flow steering, not ordinary
forwarded client traffic.

### Fresh terminal confirmation with original steering

After the final RC23 package was installed, a new Fair job ran with the
original `8,8,8,8` masks. It completed all five controls, five directional
search observations and the selected-pair confirmation. The unshaped controls
were approximately 0.886–0.896 Gbit/s download and 0.906–0.909 Gbit/s upload.
Download at the 0.8884 Gbit/s upper candidate repeated at 0.821/0.812 Gbit/s
with 100% effective CPU; the calculated 0.8753 Gbit/s floor candidate repeated
at 0.803/0.803 Gbit/s with the same CPU ceiling. Upload continued independently
and completed at a 0.9059 Gbit/s candidate, 92.4% retained capacity, class A+
and 73% peak effective CPU.

The terminal result is schema 6, `state=complete`,
`configuration_written=false`, `runtime_restored=true`, and
`recovery_pending=false`. It reports
`download:repeatable-compute-ceiling-below-capacity-floor`, retains the non-null
diagnostic download candidate, and exposes no **Apply SQM** action. Fair's
separate clean no-SQM control was class A+ and about 9.8%/8.0% faster than the
confirmed shaped pair, so the only Review actions are **Keep current** and the
explicit manual **Disable SQM** comparison. Auto-Apply remains false. The
original primary and backup qdiscs and all cake/SQM/mwan3 configuration were
restored after the job.

### Package and browser regression

The RC23 x86_64 daemon and noarch LuCI APKs were installed first on the
disposable router and then, without a network/mwan3 restart or another heavy
test, on the existing dual-WAN router. Package replacement preserved all
configuration hashes, managed qdisc rates, the restored RPS masks and both
running instances. Authenticated Playwright at 1500 pixels and 390 pixels
covered Status, Graphs, Settings, Traffic priorities, Edit and Re-run
Auto-Tune. The disposable and dual-WAN layouts rendered 2 and 4 canvases,
respectively, with zero horizontal overflow and no page, console or RPC error.
Evidence is retained in:

- `/home/w0w/cake-autorate-rs-owrt/test-logs/rc23-playwright-disposable-final`;
- `/home/w0w/cake-autorate-rs-owrt/test-logs/rc23-playwright-77-final`;
- `/home/w0w/cake-autorate-rs-owrt/test-logs/rc23-playwright-77-postrun` after
  the fresh terminal confirmation and runtime restoration.

The locally validated x86 artifacts are:

| Artifact | SHA-256 |
|---|---|
| x86_64 daemon APK | `d317bdd7a046f5af5191fe7da307d0de310f55cc08d94a83243d62d9adfa950a` |
| noarch LuCI APK | `7822059c5ba59eb442ab54111ab6524f0eb68d88f231fae62a942d4d36bee73f` |

These are development-gate artifacts, not a claim that RC23 ARM/offline release
assets have already been built or published.

## RC24 guarded-apply and managed-SQM ownership regression (2026-07-17)

RC24 fixes two related failures found after disabling and re-enabling an
existing auto-preset instance. First, LuCI could omit the hidden
`sqm_interface`, `ul_if`, and `dl_if` fields when parsing the form. Second, the
Full Auto-Tune preflight treated those convenience aliases as the ownership
root even though the canonical managed SQM section still named the correct
interface. The corrected form retains all three hidden fields. Preflight now
proves the safe section name, the `_cake_autorate_managed` owner marker, the
enabled SQM section, and its resolved target before using any convenience
alias. Automatic mode prefers the current user-selected `wan_if`; manual mode
continues to fail closed on an explicitly stale interface.

The rollback supervisor is now a separate procd service. It therefore survives
the main `cake-autorate` service reload performed by rpcd rollback, verifies
the restored snapshots, removes the exact token, and publishes the terminal
receipt. The service is boot-disabled and is started explicitly only after the
main init verifies a live transaction. LuCI confirms rpcd through the same
authenticated session that opened the rollback window; finalization then uses
a side-effect-free commit probe to prove that rpcd no longer has an armed
`apply_sid`. LuCI reloads the current page after a proved rollback, preventing
a stale in-memory CBI map from reapplying the just-rolled-back values.

Deterministic validation included the complete 156-test Rust suite, the full
Auto-Tune shell lifecycle, apply-guard and independent-init tests, LuCI
Auto-Tune JavaScript tests, all other core/LuCI shell and JavaScript suites,
packaged shell syntax, `cargo fmt --check`, and `git diff --check`. Ownership
fixtures cover missing aliases, a stale retained alias in automatic mode, a
stale explicit alias in manual mode, a mismatched managed section target, and
the normal managed CAKE/IFB path.

The x86_64 RC24 packages were then installed on the existing dual-WAN
acceptance router without restarting network or mwan3. A read-only invocation
of the packaged ownership inspector proved the existing primary instance as
`cake_wan_sqm -> pppoe-wan -> ifb4pppoe-wan`. UCI had no pending changes; both
daemon instances and all four primary/backup CAKE qdiscs remained present; no
apply-guard marker remained. No heavy speed test or calibration was run during
this regression gate.

Authenticated Playwright used fresh cache-bypassed browser contexts at
1500x900 and 390x844. It scoped Edit and Re-run Auto-Tune actions to the
primary instance row, opened the reorganized Edit form, reached the profile
step of Re-run Auto-Tune, closed both modals, navigated normally again, and
verified the installed daemon/LuCI version banner. Both viewports had exact
client/scroll widths and no page, console, RPC, stale-unsaved-change, or stale
SQM-ownership error. Evidence is retained in:

- `/home/w0w/cake-autorate-rs-owrt/test-logs/rc24-r2-final-playwright-77`;
- `/home/w0w/cake-autorate-rs-owrt/test-logs/rc24-r2-final-playwright-100`.

Locally validated development artifacts are:

| Artifact | SHA-256 |
|---|---|
| x86_64 daemon APK | `746b70be1708590d9e8bc72f85ce62a1c9520eaacebfe191898467606e272b94` |
| rockchip/armv8 daemon APK | `bdbc20d2bf7b521cff47322eabd6dd5c05a86816b16fbc28ced526daed9b41a4` |
| noarch LuCI APK | `7306a12784a9f320a1697ff39a9b621c24de4531632594a77ad1f7bd86b4700b` |

The exact packages were installed on OpenWrt 25.12.5 `rockchip/armv8` and the
production `x86/64` dual-WAN router. Both retained byte-identical UCI configs,
empty pending changes, idle Apply Guard state, healthy daemon/CAKE/IFB runtime,
and rpcd's idle `ubus` status 252. The production network and mwan3 services
were not restarted. Backups are under
`/root/cake-autorate-backups/rc24-sessionfix-20260718-075620` on each router.
These remain development-gate artifacts until tag, push and GitHub publication.

## RC25 variable-cellular safety/objective regression

RC25 separates a profile's retained-capacity objective from historical
throughput trust. An anonymized OpenWrt 25.12.5 rockchip/armv8 cellular uplink
provided the motivating Fair sample. Directional unshaped controls ranged from
about 140.2–154.8 Mbit/s download and 19.5–20.0 Mbit/s upload. A shaped
131.8/19.5 Mbit/s candidate achieved 98.1/16.3 Mbit/s: 74.4%/83.4% candidate
realization and 70.0%/83.4% observed-low retention. Loaded ICMP/transport
deltas remained within Fair's class-C target, loss was zero, the selected CAKE
qdiscs and rates were exact, and peak effective CPU was 70.9% download and
56.9% upload.

The old 90% hard-floor policy conflated two different facts. RC25 keeps
70/80/90% as fixed profile objectives and uses 50% only as an independent
historical-throughput trust warning. It does not, however, accept the 74.4%
download realization: a candidate above the current radio bottleneck cannot
prove that CAKE owns the queue. The bounded search repeats that evidence,
steps down, and tests the lower candidate. Only a subsequent 80–110% controlled
result may be offered for manual review. Auto-Apply still requires target
quality and the profile objective. CPU threshold crossings remain typed `WARN`
diagnostics and do not influence selection or Apply eligibility.

Deterministic coverage includes the exact cellular ratios, a stable
low-realization step-down and controlled retest, a below-50% manual trust warning, independent
DL/UL search, schema-7 LuCI validation, scheduler rejection of an unmet profile
objective, scheduler acceptance of an exact CPU warning, and guarded manual
Apply/rollback. It also covers bounded variable evidence with no repeatable
pair, rejection when any peer fails hard loss/quality gates, strict-profile quality
enforcement, and conservative use of the worst clean sample to seed the lower
candidate. The RC25 source gate completed 166 Rust tests, the full
Auto-Tune lifecycle, all LuCI JavaScript tests, and the init, routing,
Multi-WAN, classifier, recovery, graph-history and Apply Guard shell suites.

A previous Fair run on the same anonymized cellular router completed under the
earlier advisory interpretation. Its repeatable 136.8 Mbit/s download
candidate achieved only about 104.9 and 101.4 Mbit/s. RC25 now deliberately
rejects that candidate and retests lower instead; final live acceptance is
recorded only after the controlled-candidate build is installed.

The final RC25 armv8 build completed a fresh Fair run on the same anonymized
volatile 5G uplink. The bounded search selected 109.4/21.0 Mbit/s and achieved
90.4/17.9 Mbit/s. Current-candidate realization was 82.6%/85.1%, so both hard
CAKE-control gates passed. Retention against the earlier directional raw
sample was only 68.0%/85.1%; those gates were emitted with `required=false`
and made the class-B `latency-safe-throughput-advisory` result manual-only
instead of rejecting it. No UCI configuration was written. The original
114.5/15.8 Mbit/s CAKE queues, daemon, scheduler and recovery monitor were
restored, and pre/post `cake-autorate` and `sqm` hashes matched exactly.

The same terminal was 132,727 bytes because it retained nine shaped search
observations. A real LuCI `file.exec` request returned the complete JSON on
desktop and mobile. Playwright verified daemon/LuCI `1.0_rc25-r1`, a reviewable
manual result, no browser errors, and no horizontal overflow at 1500 px or
390 px. The audit artifacts are retained under
`/home/w0w/cake-autorate-rs-owrt/test-logs/rc25-playwright-100-final`.

The final x86_64 build was then installed on the existing Multi-WAN router.
The four current cake/SQM/network/mwan3 configuration hashes matched exactly
before and after installation. Both instance daemons plus the scheduler and
recovery monitor were active; existing primary and backup CAKE rates remained
unchanged. Desktop/mobile Playwright passed Status, Edit, Re-run Auto-Tune and
ordinary post-upgrade navigation at 1500 px and 390 px with no console errors,
stale LuCI constructor, access failure or overflow. Artifacts are under
`/home/w0w/cake-autorate-rs-owrt/test-logs/rc25-playwright-77-final`.

Final individual APK SHA256 values are:

- x86_64 daemon: `26d897888eb64a21bafcd2fb000d111b83c83f9bbf69bd2ceecc28558dcfb557`;
- aarch64_generic daemon: `fd0c4a7c8f9de7ab2ee0200a21e37c4f3ff0964881269df5f3384d6a592867b9`;
- noarch LuCI: `76f7284038d5467d485ab42b8d9f024aa1d44b0cf1953499abd330ec54d7fd26`.

The x86_64 and aarch64_generic offline repositories each contain 68 APKs.
With networking and maintainer scripts disabled, clean architecture-specific
usermode roots selected and extracted all 65 required packages. Both installer
scripts pass `sh -n`; complete manifest verification passes. Bundle SHA256
values are `547ddce52adf6873716b76074399b498250d088fdedf4334520fcb0a32046fad`
for x86_64 and
`cdc582f3609f56aeda0acf4ec76c80a12b789c32ea295bd673578576f7b24162`
for rockchip/armv8.

One earlier live run lost a pinned-server transfer during a later shaped
search point. RC25 retries `helper-exit` exactly once only at the identical
candidate, direction and server after re-proving temporary-shaper ownership
and route identity. A second exit fails closed and never reuses an older
candidate as current evidence. The deterministic lifecycle suite covers both
the successful retry and the double-failure path. No test run may write UCI
before explicit review, and the original daemon and CAKE/IFB runtime state must
be restored.

## RC26 repeated Auto-Tune monitor cleanup regression

An anonymized x86_64 Multi-WAN router exposed a lifecycle race when a completed
profile calibration was followed by a Gaming run. The first Gaming validation
passed, but a later phase was rejected with `A validation monitor could not be
terminated`. Runtime recovery still succeeded and no UCI configuration was
written.

The retained RAM evidence showed that completed transport-monitor raw files
continued growing across later phases: representative files reached 84, 68 and
50 rows although one directional speed test produced about 16–17 probe rows.
The backgrounded shell function made `$!` identify an intermediate BusyBox
`ash` process; terminating that process did not terminate its native WebSocket
probe child. Removing the CPU/ICMP marker then exposed a second race: a monitor
could exit naturally between two `/proc` checks, and absence or safe PID reuse
was incorrectly reported as cleanup failure.

RC26 makes the asynchronous shell replace itself with the native probe, so the
recorded PID owns the actual process. Cleanup considers a missing process,
zombie, or changed start time proof that the original owned monitor is gone;
it never signals the new owner of a reused PID. Deterministic tests cover both
the between-check exit and PID-reuse paths and prove that the tracked transport
PID terminates without an orphan.

The patched router then completed a fresh Gaming search at score 100 with A+
quality. Its first two download/upload raw files stopped at 17/16 and 16/16
rows, exactly one native probe existed during each phase, and none remained at
completion. The terminal reported `configuration_written=false`,
`runtime_restored=true`, and `recovery_pending=false`; pre/post hashes of the
cake-autorate, SQM, network and mwan3 configurations were identical, both
autorate instances resumed, and the original CAKE rates were restored. The
source gate also completed 166 Rust tests, the full Auto-Tune lifecycle suite,
and the scheduler and crash-recovery suites.

Both RC26 OpenWrt 25.12.5 SDK builds completed. Individual APK SHA256 values
are `9f22375b2ca3da8e0851f0806559b86a30ee276a3c8b1d2ac3a6baefa90ccee4`
for x86_64, `af33294df4305bd51a54bd6330b3dc818a6baf90249f03d3914f495cbe7a7e1f`
for aarch64_generic, and
`f30f62f3313cfd8b0d70710d0410e6321ba6bfa694d5638de52ddd38adaeb2fe`
for the identical noarch LuCI package. Both 68-package offline repositories
resolve and extract into clean architecture-specific roots without network or
maintainer scripts, and both installers pass `sh -n` and manifest validation.

The final packages were upgraded in place on the x86_64 Multi-WAN and armv8
cellular routers without changing their pre-existing cake-autorate/SQM hashes
or queue rates. Desktop and 390-pixel mobile Playwright audits verified daemon
and LuCI `1.0_rc26-r1`, Status, Graphs, Settings, Re-run Auto-Tune, Edit and
instance-scoped priorities with no JavaScript/RPC errors or horizontal
overflow. Artifacts are retained under
`/home/w0w/cake-autorate-rs-owrt/test-logs/rc26-playwright-77` and
`/home/w0w/cake-autorate-rs-owrt/test-logs/rc26-playwright-100`.

## RC26-r4 guarded Save & Apply regression

The exact Full Auto-Tune proposal is staged before the user presses the global
LuCI **Save & Apply** button. RC26-r3 incorrectly invoked the generic LuCI
form save a second time in that guarded path. Modal-only force-write controls
that were not rendered at the time could contribute their fallback value `0`;
for an enabled managed instance this could write both `enabled=0` and
`sqm_enabled=0`, remove the owned SQM queue on restart, and make the next
Auto-Tune preflight correctly refuse the disabled queue.

RC26-r4 arms the transaction directly from the already staged proposal, then
saves only the fresh guard token and its expiry. Normal settings changes with
no Auto-Tune marker retain the ordinary LuCI save path. The JavaScript
regression fixture deliberately models a generic save which attempts to set
both service flags to zero and proves that the guarded path never calls it.

The same release normalizes the two policy-number fields at the exact manifest
boundary: JSON's `70.0`/`5.0` and JavaScript/UCI's `70`/`5` compare as the same
numeric value, while a real numeric mismatch remains rejected with the exact
UCI key named in the diagnostic. The complete Apply Guard lifecycle test passes
when run sequentially; the settings test, package extraction/source-hash
comparison, shell syntax check and `git diff --check` also pass.

LuCI `1.0_rc26-r4` was installed on both the x86_64 Multi-WAN and rockchip/armv8
routers. Before/after hashes of `cake-autorate`, `sqm` and `network` were
identical on each router; their existing daemons and CAKE qdiscs remained
active. A cache-bypassed Playwright audit on the x86_64 router verified Status,
Edit and the Re-run Auto-Tune entry at desktop and 390-pixel mobile widths with
the RC26-r4 version banner, no page/console/RPC errors and no horizontal
overflow. Evidence is retained under
`/home/w0w/cake-autorate-rs-owrt/test-logs/rc26-r4-save-apply-77`.
