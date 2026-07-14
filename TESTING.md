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
bidirectional exclusion, route-change staleness, and `CURRENT`/`PREVIOUS`
lifecycle. Graph acceptance checks the
proportional RAM tiers, critical-memory suspension, shared per-instance budget,
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
`CURRENT` detected-rating lifecycle and the last completed `PREVIOUS` result;
before any result has completed, the latter explicitly reads
`No completed rating yet` rather than implying a second learning cycle.

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
- current/previous detected-rating labels and the empty-previous state;
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
