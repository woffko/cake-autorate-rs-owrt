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
2. Full Auto-Tune now requires an idle and loaded TCP/HTTPS latency signal in
   addition to fping. The larger latency delta drives its score, and either
   delta above 100 ms fails closed. `uclient-fetch` is an explicit dependency.
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
