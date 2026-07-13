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

RC6 adds a second disposable-router gate: two nftables mwan3 members, distinct
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
