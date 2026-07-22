# Full Auto-Tune

Full Auto-Tune is an experimental creation path for a CAKE Autorate instance.
The existing manual three-step wizard remains available. The automatic path
measures the selected link, calculates a complete profile-specific proposal,
displays the evidence and parameters, and writes UCI only after the user
confirms the Review step.

Full Auto-Tune also seeds transport-aware runtime control: observed-low and
median throughput become P20/P50 capacity references, the throughput guard is
enabled, and native transport-latency monitoring is enabled for the created
instance.

When the creation wizard selects every unused mwan3 uplink, calibration is
interactive and strictly sequential. Each uplink first gets its own Gaming,
Best overall, or Fair profile, then runs only through that member, and stops on
its own result. A safe shaped proposal may be accepted; any settled result may
instead be skipped. Only after that explicit decision does the wizard move to
the next uplink. A skipped uplink is shown in the aggregate Review and, if the
user finally creates the set, is written disabled with `autotune_pending=1`.
Failure, background traffic, or cancellation never auto-advances, and a member
whose runtime restoration is still pending cannot be accepted, skipped, or
bypassed. No UCI data is written until the final aggregate Review is confirmed.
Accepted members are then guarded and applied sequentially, with a complete
UCI cache reload between members; a failed or rolled-back member stops all
later transactions. Skipped members are created disabled only after every
accepted transaction has completed and no guard marker remains.

> **Current status:** Full Auto-Tune performs a bounded per-direction search over
> the measured throughput/loaded-latency boundary. Current-candidate
> realization, loss, route and measurement integrity remain hard gates. CPU,
> profile capacity objectives and comparison with an earlier volatile-link
> sample are advisory evidence. Result schema 8 separates a trusted result
> from a safe provisional or estimated result and reports capacity DL/UL,
> quality and overall confidence. Only a trusted complete target/objective
> result is eligible for unattended apply; safe lower-confidence proposals are
> explicit-review only. Each shaped phase owns the native transport-monitor PID
> directly; terminal cleanup treats an absent or safely identified reused PID
> as already stopped and never signals its new owner.

## Safety contract

- The job state and raw measurements live under
  `/tmp/cake-autorate-autotune/<job>/`; they never write router flash.
- The browser starts and polls a router-side process. Closing LuCI does not
  strand an rpcd request or partially write a configuration.
- A terminal failure first retains the complete bounded evidence. If an
  optional diagnostics fragment becomes unreadable during route loss or
  recovery, the worker stages a compact schema-valid terminal containing the
  original stage and error instead of replacing it with a generic
  "interrupted" result. Both detailed breadcrumbs and terminal history remain
  RAM-only.
- Every speed-test phase starts stopped, is identity-checked, publishes its
  process group into the recovery journal, and only then resumes in an isolated
  session. Timeout or cancel sends bounded TERM followed by KILL to the complete
  group. A helper exit code remains authoritative even when it printed valid
  JSON first.
- Phase output is written to one root-owned bounded raw file in RAM. It is
  promoted atomically only after exact exit status zero and strict validation
  that it contains one JSON object. Failed raw output remains diagnostic and
  can never become a successful sample.
- Completed/failed job snapshots are retained only in bounded RAM history.
  The default keeps at most five terminal runs and 2 MiB per instance; the
  oldest entries are discarded first and reboot clears all history.
- Preflight requires a resolved, up interface, a global IPv4 address, and a
  validated route identity. This may be the active main/default WAN or a
  specific online nftables mwan3 member whose L3 device matches the target.
- Before starting a worker, taking a runtime lock, stopping SQM, or writing a
  recovery journal, shaped validation derives its temporary identity from
  `/proc/sys/kernel/random/uuid`. The normalized value must be exactly 32
  hexadecimal characters. A colliding `catf<8-hex>` interface is treated as
  foreign and never removed; preflight retries with a new kernel UUID and then
  fails synchronously if no unique identity can be proven. Optional BusyBox
  tools such as `od` and `cksum` are not part of this contract.
- The test's own reported bytes/rate are authoritative. Router-originated test
  traffic is isolated from the nftables forwarded-client counters, so client
  background is neither added to nor subtracted from the speed-test value.
  Aggregate WAN counters are deliberately not treated as test throughput.
- A proposal is data, not an applied configuration. The current first stage
  sets `configuration_written=false`; LuCI writes it only after Review.
- Shaped validation temporarily creates a CAKE upload qdisc and download IFB at
  the proposed base rates. It refuses an interface owned by another enabled
  autorate instance or an unknown unmanaged qdisc. An existing sqm-scripts
  queue is stopped and restored through `/usr/lib/sqm/run.sh`. Before accepting
  every shaped result it verifies the exact owned root CAKE qdiscs, configured
  bandwidths, IFB, and ingress redirect again; helper flags alone are not proof
  that the candidate remained enforced for the complete test.
- A per-interface lock prevents overlapping heavy jobs. Only the selected
  instance and selected SQM queue are paused; other WAN instances continue.
- Before heavy throughput starts, a three-second RX/TX observation compares
  background use with the current directional CAKE references. The strict
  default stops at `quiet-check` above max(5%, 1 Mbit/s) and reports the exact
  DL/UL background.
- A newly created instance has no trustworthy CAKE reference yet. It therefore
  does not pretend that the link is 20 Mbit/s. Its first idle baseline is
  provisional: total interface traffic and nftables-forwarded client traffic
  are retained separately, the first routed unshaped control discovers the
  real directional capacity, and the baseline is then checked retrospectively.
  Total traffic must remain within max(5% of measured capacity, 1 Mbit/s), and
  forwarded client traffic within max(2%, 1 Mbit/s). A non-clean retrospective
  check can never become trusted or auto-applied. With explicit conservative
  consent it may continue as provisional baseline evidence; all hard route,
  measurement and safety gates still apply.
- Every unshaped and shaped speed-test phase additionally owns a temporary
  nftables forwarding counter. It counts only client traffic crossing the
  selected WAN, not traffic generated by the router-local speed-test process.
  A phase is contaminated when its forwarded background exceeds max(2% of its
  directional reference, 1 Mbit/s), or when the counter is unavailable.
- A contaminated phase is repeated once. A second contaminated attempt stops
  strict mode with `background-blocked` and offers Retry or one-run
  conservative continuation. Accepted contaminated evidence can return a safe
  lower-confidence proposal for explicit review and is never eligible for
  unattended Auto-Apply. The conservative request itself is not a permanent
  penalty: a completely clean fresh rerun can still become trusted. The
  scheduler never invokes the override.

The job tests shaped candidates on the same pinned server, independently
samples ICMP latency/loss, native persistent transport latency, aggregate CPU,
the busiest CPU core, softirq CPU and the owned CAKE counters, then rechecks
the WAN address/route before accepting an observation. The effective CPU metric
uses the greater of aggregate and busiest-core load, so one saturated core
cannot hide inside a low multi-core average. In addition to the advisory peak,
diagnostics retain mean and p95 effective CPU, sample count,
the number and longest consecutive run above the profile limit, and softirq
p95. The second latency signal is deliberate: providers may prioritize ICMP
while TCP is still badly queued. Each direction is searched independently,
but the exact selected DL/UL pair is confirmed together because both
directions can still share CPU, interrupt and memory resources.

Preflight also records a diagnostic datapath snapshot. It resolves the
selected logical/L3 path to the physical ingress device where possible, lists
every RX queue `rps_cpus` mask, records OpenWrt's Packet Steering mode, and
flags the specific case where several queues all share one single-CPU mask.
The snapshot is an indicator, not a complete affinity model: IRQ affinity,
hardware RSS, XPS, PPPoE/IFB processing and qdisc locking may also be limiting.
Auto-Tune never changes any of those settings.

## Calibration profiles

The profile is a calibration contract, not a cosmetic preset. It selects the
rate factors, loaded-delay target, throughput trust boundary, loss gate,
adaptive-ceiling cadence and SQM traffic-class policy used by proposal,
temporary shaped validation and final configuration.

| Profile | Intended balance | Target | Retained-capacity objective | Maximum loaded ICMP/transport delta | Maximum loss | SQM policy |
|---|---|---:|---:|---:|---:|---|
| Gaming | Maximum safe throughput that still proves A+; otherwise the best attainable grade | A+ | 70% | `< 5 ms` | 1% | `layer_cake.qos`; upload/download `diffserv4`; preserve DSCP |
| Best overall | Maximum safe throughput at A, with a balanced fallback | A | 80% | `< 30 ms` | 3% | `layer_cake.qos`; upload `diffserv4`; download best effort + wash |
| Fair | Maximum safe throughput first; quality breaks near-throughput ties | C (soft) | 90% | `< 200 ms` | 5% | `layer_cake.qos`; upload `diffserv4`; download best effort + wash |

All profiles also report a separate 50% historical retained-capacity trust
boundary. Crossing it forces explicit manual review; it is not a hard safety
failure on cellular/shared-medium links.
The grades are target classes, not promises about an ISP, server, Wi-Fi client
or unrelated bottleneck. Grade limits are strict: exactly 5 ms is A, not A+;
exactly 30 ms is B, not A. Every profile records the 80–110% candidate
realization interval as a hard shaper-integrity check. A repeated low value
causes a lower candidate retest rather than an applicable advisory. All profiles also
record an 85% effective-router-CPU warning threshold. The effective value is
`max(aggregate CPU, busiest-core CPU)`.
Gaming and Best overall require their target for unattended eligibility. If a
target or throughput objective is unreachable, Review can show a
manual-only fallback: Gaming chooses the best attained grade and then the
fastest candidate in that grade; Best overall chooses the strongest balanced
quality/throughput candidate. Fair treats C as a soft quality goal while its
  measurement-integrity, loss and route gates remain hard. Forwarded-counter
  availability and integrity are hard; measured background lowers confidence
  and may force manual review. The 50%
historical-throughput comparison is advisory. Fair maximizes achieved throughput and, within a
1.5% uncertainty band of the fastest safe result, prefers lower loaded delay.
When a current CAKE candidate produces class C's 200 ms boundary or worse,
the search may reduce that rate even if doing so moves farther below an older
5G capacity sample. Preserving the old sample would leave the bottleneck and
its queue in the modem, which defeats SQM.
No profile silently lowers its objective to manufacture unattended eligibility.

Best overall is the default for new jobs and for existing instances which do
not yet have `autotune_profile`. The old `balanced` CLI value is accepted as an
alias for `best_overall`, but all new results use the canonical name.

With native traffic rules disabled, Gaming can use trusted client upload and
WAN-ingress markings. The optional outbound rule editor instead establishes a
deterministic upload policy: it resets outbound DSCP to CS0 and then matches
only explicitly configured profiles/presets/ports/addresses. It does not
install qosify/eBPF or infer an application from traffic contents. Gaming
continues to preserve WAN-ingress DSCP instead of washing it, so use Best
overall when downstream markings are not trusted. Best overall and Fair wash
download markings but can still apply their native profile rules before
upload CAKE.

The classifier is not a second shaper. It owns only its private nftables table;
CAKE, IFB, redirects, and rates remain under the one managed SQM owner. Custom
rules relevant to the selected instance are part of the calibration
configuration fingerprint, so changing them invalidates stale Review or
Scheduled Auto-Apply evidence. See
[Profile traffic priorities](TRAFFIC_PRIORITIES.md).

## Job phases

1. `preflight`: check the native daemon/probe helper, backend, interface,
   address, structured `main`/`mwan3` route identity, and link encapsulation.
2. `reflectors`: run the existing reflector planner and require three reachable
   RTT targets from independent provider/address families. Three addresses in
   one anycast service do not satisfy this requirement.
3. `baseline`: collect ICMP RTT plus at least 15 accepted native transport RTT
   observations. The WebSocket/HTTPS transport process reuses its connection;
   DNS, process startup, TCP/TLS, and protocol handshakes are not scored. For a
   new uncalibrated instance the baseline remains explicitly provisional until
   the first raw-capacity control proves its relative traffic limits.
4. `quiet-check`: measure pre-test background and either proceed, stop for a
   quiet retry, or require explicit one-run conservative consent.
5. `throughput`: run one bidirectional unshaped control, retrospectively accept
   or reject a provisional baseline, then run two
   download-only and two upload-only controls. `speedtest-go` validates the
   first good automatically selected server; its ID is a job-local hard pin
   for every later control and shaped observation. The proposal receives three
   direction-matched values per direction (the bidirectional control plus its
   two directional controls). Fair also retains the simultaneous control as
   the possible no-SQM comparison.
6. `proposal`: call the Rust daemon's pure `--autotune-proposal` mode.
7. `shaped`: temporarily apply CAKE at proposed base rates and test download
   and upload separately on the same server with SQM bypass disabled. Each
   observation records rate-limited ICMP RTT/loss, persistent transport RTT,
   aggregate/busiest-core/softirq CPU, owned CAKE counters and
   forwarded-background evidence.
8. `validation`: call the Rust `--autotune-validate` mode. It publishes each
   gate and all three directional throughput ratios; the typed profile
   optimizer consumes the resulting directional observation.
9. `profile-search`: independently evaluate up to eight shaped observations per
   direction. Repeat one candidate when realization is unreliable; raise a
   clean under-retaining candidate to `ceil_100(O*F/r)`; explicitly test the
   observed-low upper bound; and bisect a discovered quality/safety boundary
   to 0.5% of observed-low capacity. A reliable but CPU-unsafe point is also
   repeated at the exact rate, checked at the upper bound, and—when no safe
   point exists—checked at the calculated hard-floor candidate. A completed
   direction is frozen while the other continues. The exact selected pair is
   then confirmed together.
10. `review`: return raw runs, every validation attempt, baseline, reflector
    plan, route identity, phase-background evidence, detected link, and either
    a passing proposal or the explicitly typed Fair manual choices.

The RPC-facing helper uses the same explicit route and profile identity for
start, polling, cancellation and live attestation:

```text
/usr/libexec/cake-autorate-rs/autotune JOB INTERFACE MODE BACKEND \
  [ROUTE_MODE] [MWAN3_MEMBER] [PROFILE] [CONSERVATIVE]
```

`MODE` is `start`, `start-conservative`, `status`, `cancel`, or the internal
attestation mode used by LuCI. `PROFILE` is `gaming`, `best_overall`, or
`fair`. RC17 callers which supplied the conservative flag as argument seven
remain compatible and resolve to Best overall.

## Background traffic and conservative continuation

Retrying on a quiet link is the preferred action. **Continue conservatively**
is an explicit override for the current job only; it is not saved as an
instance setting and scheduled calibration remains strict.

The speed-test helper reports only its own measured transfer. Background is
therefore evidence about confidence and competition for capacity, not a value
to subtract from (or add blindly to) that isolated sample. The proposal keeps
the measured directional rates, applies the profile's normal safety bounds,
and prevents retained settings from exceeding the newly confirmed maximum or
absolute adaptive-ceiling cap.

Result schema 8 reports four confidence percentages: download capacity,
upload capacity, quality, and `overall`, which must equal the weakest of the
other three. Background share is calculated against the corresponding
directional reference. A contaminated baseline lowers quality confidence;
accepted phase contamination lowers the affected capacity confidence and is
retained as a typed reason. Classes are:

- `trusted`: at least 85%, complete clean phase evidence; the only class that
  may satisfy scheduled Auto-Apply;
- `provisional`: 40% through 84%; a safe proposal may be accepted manually;
- `estimated`: below 40%; still manual-only and displayed with a strong retry
  recommendation.

The class never overrides hard failures. Route/member/source identity,
external-IP continuity, SQM ownership and exact temporary rates, malformed or
missing directional samples, missing forwarded counters, loss/latency safety,
runtime restoration and immutable configuration identity remain blockers.
LuCI offers **Retry for higher confidence**, **Accept safe proposal** when the
server-side manual gate is true, or **Skip this uplink** in the sequential
Multi-WAN flow. Scheduled Auto-Apply never consumes provisional or estimated
results.

The phase counters are independent of that initial quiet check. Their evidence
is stored with the run as `available`, `contaminated`, duration, observed DL/UL
kbit/s, and both limits. Missing counters are not interpreted as zero traffic.
If validation does not pass, current settings remain active and UCI is
untouched.

## Profile proposal mathematics

Invalid, zero, NaN, and infinite throughput samples are rejected. For each
direction the samples are sorted. With up to three samples, the observed low is
the minimum; with more samples it is p20. The reported centre is the median and
the observed high is p90 using linear interpolation.

For direction `d`:

```text
variability_d = (high_d - low_d) / max(median_d, 1)
variable      = variability_DL >= 0.15 or variability_UL >= 0.15
```

For each profile, the tuple below is
`(minimum / low, base / low, maximum / high, cap / high)`:

| Profile | Stable direction | Variable direction |
|---|---|---|
| Gaming | `(0.60, 0.82, 0.92, 1.02)` | `(0.35, 0.75, 1.20, 1.60)` |
| Best overall | `(0.70, 0.88, 0.95, 1.05)` | `(0.40, 0.85, 1.25, 1.80)` |
| Fair | `(0.35, 0.94, 0.98, 1.08)` | `(0.35, 0.92, 1.30, 1.90)` |

Rates are rounded to 100 kbit/s and then constrained to
`minimum <= base <= maximum <= cap`. The deliberately wide variable maximum is
not the starting shaper: the shaper starts at `base`, while `maximum` and the
absolute cap leave bounded room for the inner controller and adaptive-ceiling
probes on a recovering radio link.

Activity detection is one tenth of the weaker observed-low direction, rounded
to 100 kbit/s and clamped to 500..20000 kbit/s. This keeps low-rate uploads
visible without treating tiny background traffic as an active connection.

For idle RTT median `m`, p95 `p`, and `j = max(p - m, 0)`, the runtime
threshold proposal is profile-specific:

```text
Gaming:
  adjust-up   = ceil(clamp(j, 1, 3)) ms
  delay       = 5 ms
  adjust-down = 20 ms

Best overall:
  adjust-up   = ceil(clamp(1.5 * j, 3, 15)) ms
  delay       = max(adjust-up + 8, 15) ms
  adjust-down = max(delay + 25, 40) ms

Fair:
  adjust-up   = ceil(clamp(2 * j, 5, 20)) ms
  delay       = max(adjust-up + 15, 30) ms
  adjust-down = max(delay + 30, 60) ms
```

Adaptive ceiling is enabled automatically only when either direction is
variable. The proposed
`hold / growth / observation / cooldown / failed-bound TTL` values are:

| Profile | Policy |
|---|---|
| Gaming | `30 s / 1% / 8 s / 90 s / 1800 s` |
| Best overall, variable | `15 s / 3% / 8 s / 45 s / 900 s` |
| Best overall, stable | `20 s / 3% / 8 s / 60 s / 1800 s`, disabled |
| Fair | `10 s / 5% / 10 s / 30 s / 600 s` |

Detected PPPoE uses Ethernet framing, overhead 44, and MPU 84. Plain Ethernet
uses overhead 18 and MPU 64. Cellular links use raw/no-overhead defaults;
unknown encapsulation produces a Review warning rather than guessing.

## Proposal input completeness

The Rust proposal's legacy numeric `proposal.confidence` describes only whether
the calculator received complete inputs:

- up to 60 points for three valid samples in both directions;
- 25 points for at least five valid idle RTT samples;
- 15 points for detected encapsulation.

The score is capped at 100. Missing baseline, a single throughput sample,
variable capacity, or unknown link layer also emits a human-readable warning.
It is neither schema-8 result confidence nor apply eligibility. The latter is
the four-dimensional background-aware envelope described above and remains
separate from the shaped validation score.

## Shaped validation mathematics

For each direction let `O` be the unshaped observed-low capacity, `C` the CAKE
candidate, and `A` the shaped test's achieved throughput. The validator
deliberately keeps three different ratios:

```text
candidate_realization = 100 * A / C
capacity_retention    = 100 * A / O
candidate_capacity   = 100 * C / O
```

`candidate_realization` answers whether the test actually exercised the
configured candidate. `capacity_retention` answers how much proven link
capacity remained usable. `candidate_capacity` describes how conservative the
candidate itself is. None is a substitute for another, and the UI labels all
three separately.

The hard gates are independent of the diagnostic score. The packaged job keeps
the full 80..110% candidate-realization interval hard: below 80% CAKE may sit
above the actual bottleneck, while above 110% the claimed temporary shaper is
not credible. The 70/80/90% retention values are profile objectives and the
50% historical-retention trust boundary plus effective peak CPU above 85% are
advisory. Latency/loss policy, route and measurement integrity also remain
hard. Proposal schema 3 carries the
canonical profile, target grade, whether that target is a hard requirement,
the throughput-priority flag, exact validation thresholds and complete SQM
recommendation. Result schema 8 binds that policy, both typed per-direction
search histories, confidence envelope, selected pair, immutable run identity,
phase evidence and recovery state to the job identity and configuration
fingerprint. The standalone Rust CLI has defensive fallback
thresholds, but the job always passes the proposal's validated profile values
explicitly. The diagnostic score is the worst normalized gate margin: a
minimum gate contributes `100 * actual / limit`, a maximum gate contributes
100 while it passes and `100 * limit / actual` after it fails, and the lowest
contribution wins. It is clamped to 0..100. Thus the score identifies the
tightest constraint but never overrides a failed gate.

Temporary validation mirrors the final SQM policy. Gaming creates upload and
IFB CAKE qdiscs with `diffserv4 nat` and without `wash`. Best overall and Fair
create upload CAKE with `diffserv4 nat`; their download IFB uses
`besteffort nat wash`, matching the final asymmetric `layer_cake.qos` policy.
PPPoE validation uses Ethernet overhead 44 and MPU 84; plain Ethernet uses
overhead 18 and MPU 64. The verifier rejects the shaped result if any class,
wash mode, rate, link-layer token, IFB or ingress redirect differs.

Idle and loaded transport use the same quantile:

```text
transport_delta_p95 = max(loaded_p95 - idle_p95, 0)
icmp_delta_p95      = max(loaded_p95 - idle_p95, 0)
effective_delta     = max(transport_delta_p95, icmp_delta_p95)
```

The old `loaded p95 - idle median` calculation inflated ordinary baseline
jitter and is not used. Every valid raw persistent-transport observation is
kept for the phase percentile, including a legitimate high tail; the robust
median is diagnostic only and cannot erase queueing spikes. Loaded ICMP runs at
no more than one batch per second, uses at least three independent reflector
families, and reports median per-reflector loss so one anycast service's rate
limiter cannot decide the result. A reflector set that becomes rate-limited
during load invalidates the measurement instead of being silently re-baselined.

Full Auto-Tune accepts only persistent WebSocket or persistent HTTPS evidence.
The native TCP-connect backend remains useful for diagnostics, but a fresh TCP
handshake per sample cannot provide the required warmed-session latency and is
rejected before baseline or throughput work starts.

The Rust profile optimizer derives the candidate needed to retain the safety
floor fraction `f` at the measured realization
`r = candidate_realization / 100`:

```text
required_floor = ceil_100(O * f / r)
```

All rate results are rounded upward to 100 kbit/s. The search upper bound is
the direction's observed-low capacity, not the proposal's initial maximum.
Its actions are:

- `test`: measure a new bounded candidate, or collect up to three observations
  at one candidate when realization is outside 80–110%; repeatable or bounded
  variable low realization causes a lower candidate to be tested so CAKE is
  proven to control the bottleneck;
- `complete`: a controlled safe optimum was bounded or confirmed; it may be
  manual-only when a retention objective or historical trust boundary is
  missed;
- `fallback`: trustworthy evidence produced at least one candidate for manual
  review, but the required Gaming/Best overall target was not proved;
- `inconclusive`: measurement reliability could not be established.

For Gaming and Best overall the search first finds a target-grade point, tests
toward the upper bound, and bisects between the highest target pass and the
first higher target failure. Gaming fallback orders safe points by grade and
then achieved throughput; Best overall fallback uses an equal-weight bounded
quality/retention score. Fair explicitly tests the upper bound and maximizes
safe achieved throughput; candidates within 1.5% of that maximum are ordered
by lower effective loaded delay and then throughput.

A fallback is never permission to bypass latency, loss, route or measurement
integrity. Failed or incomplete runs remain diagnostic-only. When a
background-aware run retains contaminated evidence, it can become a
manual-only provisional/estimated proposal only after the same final safety
and structured-evidence checks pass. A fresh clean retry may become trusted.
Main-route
jobs require the target to be the active default device. Structured Multi-WAN jobs route ICMP, native
transport sockets, and the selected speed-test backend through the same
validated member. Baseline and loaded observations remain bound to the selected
route identity; throughput and shaped phases additionally recheck external IPv4
and the pinned speed-test server. Missing telemetry or a relevant identity
change fails closed and leaves UCI untouched.

Measurement integrity failures are not presented as a proved bad candidate.
Missing or untrusted transport evidence, reflector rate limiting, loss of the
temporary shaper, an over-realized candidate after its bounded retry, and
similar ambiguous observations end as `INCONCLUSIVE` with an
explicit retry reason. `FAILED` is reserved for a valid complete measurement
which proves that hard quality/integrity limits have no legal intersection.
Neither state exposes Apply or replaces the active settings,
except for the specific complete Fair hard-safe outcome described above;
measurement-integrity failures never qualify for it.

A low-realization result is not automatically blamed on the ISP. The search
collects as many as three samples at the same candidate and first looks for an
achieved-rate pair within 5%. It then derives a lower bounded candidate from
the worst clean achieved sample and tests that rate. The original candidate is
never apply-eligible because it has not proved that CAKE is below the physical
bottleneck. Once the lower candidate realizes at least 80%, a missed retention
objective is manual-only; crossing 50% adds a historical-throughput warning.

Cellular, Wi-Fi and some shared-medium links can remain clean while three
same-candidate throughput results vary by more than 5%. After that bounded
retry, the worst clean achieved sample likewise seeds a lower candidate. Only
the subsequent controlled retest can finish the search. A retention shortfall
then requires explicit manual confirmation and never authorizes scheduled or
automatic Apply.

RC25 treats the CPU threshold as advisory. CPU peak, aggregate CPU, busiest
core, softirq, mean, p95, over-limit count and longest over-limit run remain in
the result, but CPU alone cannot change search action or selection, fail final
validation, request a rate correction, make a capacity floor infeasible, or
block manual/scheduled Apply. The typed CPU gates remain visible with
`required=false`; threshold crossings are emitted in `warnings` and shown as
`WARN`. A non-CPU resource failure such as excessive loss is still repeated at
the exact candidate and becomes `INCONCLUSIVE` if unresolved; it never becomes
an applicable fallback.

An observation which fails a hard loss, route, latency or measurement-integrity
gate remains diagnostic evidence, not a safe configuration. It can never
expose **Apply SQM** or be consumed by Scheduled Auto-Apply.

## Fair Review outcomes

Fair separates a throughput-first decision from unattended quality approval:

1. **Apply the best safe Fair SQM candidate** is automatic only when both the
   90% throughput objective and class C are met. A clean candidate that misses
   either objective or the 50% historical trust boundary is an explicit manual
   choice.
2. **Keep current settings** closes a re-run without writing configuration, or
   cancels creation of a new instance.
3. **Disable autorate and SQM** appears only for an existing managed instance
   whose shaped result remains above the 50% historical trust boundary, and
   when a simultaneous bidirectional unshaped control is valid, SQM was proved
   paused, no temporary shaper existed, forwarded-background counters were
   available and clean, the no-SQM grade is no worse, its effective delay is
   no more than 10 ms above the shaped candidate, and both download and upload
   improve by at least 2%.

The third action is a comparison suggestion, never the preselected choice. A
candidate below the historical trust boundary retains **Apply SQM** for manual
review but suppresses **Disable autorate and SQM**.
The user must select it and confirm a red warning. Apply Guard then preserves
the instance and owned queue configuration as disabled, restarts the normal
service transaction, and proves that no instance daemon, target/IFB CAKE qdisc,
ingress/clsact redirect or owned IFB remains. Any ambiguous postcondition rolls
back. Scheduled Auto-Apply can never choose this action.

When an eligible Review action is applied, LuCI uses a guarded UCI transaction rather
than treating a browser RPC success as sufficient proof. The guard snapshots
the complete `cake-autorate` and `sqm` packages, starts the normal rollback
window, verifies the exact daemon/qdisc/IFB/redirect runtime, removes its
temporary enrollment markers inside that same rollback window, and then asks
rpcd to confirm through the same authenticated LuCI session that started the
transaction. The root supervisor never attempts to impersonate that session.
Only after rpcd has synchronously closed the rollback transaction may the
supervisor finalize and remove its receipt. A missing or ambiguous
confirmation is reconciled against exact
pre-change and marker-free expected-final fingerprints; an indeterminate state
remains recoverable rather than being declared successful.

Multi-WAN deliberately keeps this guard single-proposal. LuCI serializes the
aggregate Review into independent guarded transactions rather than arming
several route/SQM owners at once. This preserves exact rollback and runtime
attestation for every member and avoids stale client tokens by unloading and
reloading `cake-autorate` and `sqm` after each confirmed transaction.

RC20 additionally binds the transaction to the current boot identity and an
immutable supervised receipt. Service start performs the Apply Guard preflight
before any SQM or daemon mutation. It either verifies the live transaction,
recovers a provably stale boot/transaction pair, or refuses the start; a
partial persistent marker can no longer be mistaken for a successful apply.

RC24 runs that receipt supervisor in its own `cake-autorate-apply-guard` procd
service. This separation is a correctness requirement: rpcd rollback reloads
the main `cake-autorate` service, so a supervisor registered as one of its
instances would be killed before it could verify the restored snapshots and
publish the terminal receipt. The independent service derives the transaction
from `verify-init` itself, validates the root-owned token, and never trusts a
token supplied by LuCI or by the main service. A proved rollback also reloads
the LuCI page immediately so stale in-memory enrollment markers cannot appear
as ordinary unsaved changes and be committed a second time. This token-driven
helper is intentionally disabled at boot; the main service starts it only for
a verified live transaction. Enabling it permanently would add no crash
recovery and would emit a false missing-token warning on normal boots.

Auto-managed `sqm_interface`, `ul_if`, and `dl_if` values are retained while
their form controls are hidden. They are still not treated as ownership proof:
preflight first validates the safe SQM section name, its
`_cake_autorate_managed` owner and the queue target. In automatic mode the
selected `wan_if` is authoritative; manual interface aliases take precedence
only when `auto_interface_preset=0`.

## Tests

Rust unit tests cover every stable/variable profile matrix, profile aliases,
Gaming `diffserv4`, invalid samples, JSON output, three-ratio separation, typed
gates, unreliable-measurement repeat, repeated floor-seeking increases,
quality-boundary bisection, all three profile orderings, strict grade
boundaries, Fair target/fallback separation and no-SQM comparison. RC23 adds
the reliable-realization CPU-saturation regression, exact-rate CPU repeat,
upper/floor probes, continued asymmetric upload search, non-null diagnostic
selection and fail-closed repeated non-CPU resource failure.
The shell lifecycle test uses isolated mock helpers
to verify profile/job binding, exact temporary CAKE tokens, progress/result
output, job-local server pinning, reflector diversity, one-second ICMP pacing,
persistent transport parsing, phase-background evidence, bounded RAM history,
valid-JSON/nonzero-exit rejection, atomic result publication, process-group
timeout/cancellation, kernel-UUID temporary identities, collision retry,
missing/invalid UUID sources, orphan cleanup, recovery, and both speed-test
and temporary shaper cleanup traps. It also forces detailed terminal staging
to fail and requires a compact schema-valid terminal with the original failure
reason. Apply Guard tests reject incomplete,
contaminated, one-direction, below-2%-gain and runtime-residue disable
attempts. Real-router acceptance also checks per-member route identity and that
the unselected autorate/SQM instance continues running.

## Optional scheduler

`cake-autorate-autotune` is a lightweight procd service. Per-instance
`scheduled_autotune_*` options select interval, local hour window, required
quiet time, RAM-only daily traffic budget, and whether a validated result is
automatically applied. The feature defaults off, and auto-apply defaults off.
The scheduler reuses the exact preflight, route identity, five raw controls,
same-server directional search, selected-pair confirmation, cleanup, and fail-closed
result described above. Auto-Apply additionally requires result schema 8,
`state=complete`, `result_class=trusted`, overall and quality confidence of at
least 85%, final `validation.pass=true`, both quality targets met, and no phase
contamination. Provisional, estimated and profile-fallback results are never
scheduled. A CPU warning by itself does not block scheduling; the scheduler
still requires the exact passing gate set, no correction, complete clean phase
evidence and restored runtime. It also requires an exact match between the instance's
saved profile and the profile, target, gate set and SQM policy inside the
current result. Re-running an existing instance preserves the user's explicit
Adaptive Ceiling enabled/disabled choice; a calibration proposal does not
silently flip it. See [TRANSPORT_QUALITY.md](TRANSPORT_QUALITY.md) for the
runtime safeguards and [MULTIWAN.md](MULTIWAN.md) for routing behavior.
