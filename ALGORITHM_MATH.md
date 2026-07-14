# Controller Mathematics

This document describes the implemented controller rather than an idealized
network model. Source code remains authoritative. The controller has two
independent layers for download and upload:

1. a fast, upstream-compatible load-and-delay loop that selects the current
   CAKE rate; and
2. an optional slow bounded-probe controller that decides how high the fast
   loop may search above the configured maximum.

The second layer is a Rust extension. With `adaptive_ceiling_enabled=0`, the
configured maximum remains a hard limit, matching upstream behavior.

An additional optional transport RTT layer is documented in
[TRANSPORT_QUALITY.md](TRANSPORT_QUALITY.md). Measurement and detected grading
are observational; only the separate default-off
`transport_controller_enabled` option permits transport evidence to influence
CAKE. When enabled, its effective delay is the maximum of directional ICMP/OWD
growth and confirmed native network RTT growth. Missing evidence blocks that
optional controller's growth but never fabricates bufferbloat or cuts the rate.
A twice-confirmed high directional transport delta uses a bounded square-root
correction and may not cross the robust throughput floor:

```text
factor    = clamp(sqrt(target_delay / measured_delay), 0.70, 0.97)
candidate = max(throughput_floor, current_rate * factor)
```

## Notation

For one direction, let:

- `R` be the achieved traffic rate in kbit/s;
- `C` be the current CAKE shaper rate in kbit/s;
- `L = 100 R / C` be load in percent;
- `B` be the configured baseline rate;
- `Rmin` be the configured hard minimum;
- `M` be the configured maximum and initial ceiling;
- `E` be the effective runtime ceiling;
- `S` be the highest ceiling confirmed safe by a completed probe;
- `F` be the lowest remembered failed ceiling, if any;
- `A` be the absolute adaptive ceiling cap;
- `D_i` be a directional OWD sample in microseconds;
- `Q_i` be that reflector's moving delay baseline;
- `Delta_i = D_i - Q_i` be the queueing-delay estimate.

All rate-control state is separate for download and upload. The formulas below
are therefore applied twice with direction-specific values.

## Uplink partition and route lifecycle

In Multi-WAN mode, controller state is partitioned by instance and route
identity. For uplink `u`:

```text
I_u = (route_mode, member, L3_device, source_IP, fwmark, routing_table)
S_u = (delay_baselines, transport_baselines, throughput_reference,
       quality_state, DL_ceiling_state, UL_ceiling_state)
```

There is no transition that copies `S_u` to another uplink. When `I_u` changes,
or the member goes offline and later recovers:

```text
S_u := initial_learning_state
lifecycle_u := LEARNING
```

Other `S_v`, where `v != u`, are unchanged. After the configured route
stability interval and sufficient fresh samples, lifecycle becomes `ACTIVE` if
the member has a non-zero share in the default mwan3 policy, otherwise
`STANDBY`. An unavailable or mismatched member is `OFFLINE`; its pingers are
stopped and it cannot accumulate reflector offences or promote a ceiling.

Transport confidence is intentionally staged. Twenty accepted idle network RTT
samples contribute the first half, so the UI reports `BASELINE READY` while
waiting for natural loaded samples. Twenty accepted loaded samples in a
direction complete that half. This is a stable waiting state, not a stalled
learning loop.

The separately displayed detected rating follows the current LibreQoS browser
test statistics. The native backend resolves DNS and warms its TCP/TLS/
WebSocket or HTTP session before timing; legacy whole-process HTTP is untrusted.
For selected endpoint `e`, direction `d`, idle network RTT samples `I_e`, and
loaded network RTT samples `J_d`:

```text
B_e       = percentile(I_e, 0.05)
P_d       = percentile(J_d, 0.90)
raw_d     = P_d - B_e
delta_d   = 0                    if abs(raw_d) < 2 ms
            max(raw_d, 0)       otherwise
grade     = max_severity(grade(delta_DL), grade(delta_UL))
```

Percentiles are linearly interpolated between adjacent sorted observations.
Bidirectional samples are reported but excluded from `grade`. The boundaries
are A+ for `delta < 5`, A for `< 30`, B for `< 60`, C for `< 200`, D for
`< 400`, and F otherwise. At least 20 idle samples and 20 loaded samples per
scored direction are required. A one-direction result is `PARTIAL` and is never
shown as a final grade. `CURRENT` is the active/latest result; `PREVIOUS` is the
last completed episode and remains visible while another episode collects. An
episode finalizes after 30 seconds without accepted directional loaded samples
by default. All
these samples are scoped to `I_u`; after a route change the retained result is
explicitly stale and cannot be combined with new samples.

All ICMP, transport, speed-test, and Auto-Tune observations are accepted only
when their route identity equals the instance identity:

```text
accept(sample) iff sample.route_identity == I_u
```

For calibration, external IPv4 and speed-test server ID are also held constant
between phases. See [MULTIWAN.md](MULTIWAN.md) for the operational state
machine.

## Achieved rate and load

The daemon samples Linux interface byte counters. Over an elapsed interval
`dt` seconds:

```text
R = 8 * max(bytes_now - bytes_previous, 0) / (1000 * dt)
L = 100 * R / C
```

Download uses the configured RX counter and upload uses TX. Counter rollback
is saturated at zero. An interval is never treated as shorter than 1 ms.

The fast rate controller classifies load as:

```text
high  if L > 100 * high_load_thr
low   if not high and R > connection_active_thr_kbps
idle  otherwise
```

The packaged `high_load_thr` default is `0.75`, or 75% of the current CAKE
rate.

## Independent rating-load state machine

Detected grading must recognize sustained routed client tests without inheriting
the fast controller's instantaneous transitions. For each direction, RC9 keeps
all `(R, R/C)` observations in a window of length `W` and calculates:

```text
Rbar_d = mean(R_d)
Lbar_d = mean(clamp(R_d / C_d, 0, 10))
```

Let `E` be the enter ratio, `X` the exit ratio, `K` the minimum absolute rate,
and `D` the direction-dominance ratio. With defaults `W=2 s`, `E=0.60`,
`X=0.40`, `K=2000 kbit/s`, and `D=1.5`:

```text
loaded_d(E) = (Lbar_d >= E) and (Rbar_d >= K)

phase = IDLE          if neither direction is loaded
        DL            if only DL is loaded
        UL            if only UL is loaded
        DL            if both and Lbar_DL >= D * Lbar_UL
        UL            if both and Lbar_UL >= D * Lbar_DL
        BIDIRECTIONAL otherwise
```

A new candidate must persist for one second before the phase changes. A loaded
phase remains supported using `X` and `K/2`; only 1.5 seconds continuously below
that lower boundary returns it to idle. Thus the enter/exit gap is true
hysteresis, not two independent labels. A transport batch is admitted to a
directional grade window only when its route identity and rating phase still
match at completion.

The optional bounded capture marker used by `Get rating` does not inject
samples. It learns the highest smoothed ratio `P` seen during that job and uses:

```text
E_capture = min(E, max(0.15, 0.55 * P))
X_capture = min(X, max(0.10, 0.67 * E_capture))
```

Removing the marker immediately restores normal passive thresholds. Controller
classification and all CAKE update equations remain unchanged.

## Delay samples and moving baseline

Directional timestamp backends provide separate download and upload OWD.
RTT-only backends estimate both directions as half of RTT:

```text
D_dl = D_ul = RTT / 2
```

Each reflector has independent directional baselines. For every valid sample:

```text
alpha = alpha_baseline_increase  when D_i >= Q_(i-1)
alpha = alpha_baseline_decrease  when D_i <  Q_(i-1)

Q_i     = alpha * D_i + (1 - alpha) * Q_(i-1)
Delta_i = D_i - Q_i
```

The asymmetric defaults (`0.001` upward, `0.9` downward) let the baseline fall
quickly when a cleaner path is observed but rise only slowly into persistent
queueing delay.

## Packet serialization compensation

At low rates, one maximum-size packet can consume a meaningful fraction of a
delay threshold. The daemon derives maximum wire size from interface MTU and
the live CAKE link-layer/overhead settings.

For non-ATM links:

```text
P_bits = 8 * (MTU_bytes + overhead_bytes)
T_serialization_us = 1000 * P_bits / C_kbps
```

ATM mode applies 53/48 cell rounding before the same rate conversion. The
serialization time is added to the per-sample delay threshold and to the
average-delay up/down thresholds. This prevents normal packet transmission
time from being misclassified as bufferbloat.

## Bufferbloat detector

The daemon retains a window of `W = bufferbloat_detection_window` booleans and
delay deltas. For each sample:

```text
b_i = Delta_i > (owd_delta_delay_threshold + serialization_compensation)
count = sum of b_i over the current W-sample window
bufferbloat = count >= bufferbloat_detection_thr
avg_delta = average(Delta_i over the same window)
```

The packaged defaults use a six-sample window and require three offences. A
single delayed reflector response therefore does not immediately cut the
shaper. Reflector health checks separately remove persistently late or
misbehaving reflectors.

## Fast inner rate controller

### Confirmed bufferbloat

Let `Tdelay` be the compensated per-sample delay threshold and `Tdown` the
compensated average-delay threshold for maximum reduction. Define:

```text
x = clamp((avg_delta - Tdelay) / (Tdown - Tdelay), 0, 1)
m = down_min - x * (down_min - down_max)
C_next = C * m
```

With the packaged multipliers, `m` ranges from `0.99` for a marginal event to
`0.75` for a severe event. Reductions are limited by the bufferbloat
refractory period, 300 ms by default.

### Clean high load

When load is high and the detector is not in its refractory period, let `Tup`
be the compensated maximum average delay that permits growth:

```text
y = clamp((Tdelay - avg_delta) / (Tdelay - Tup), 0, 1)
g = up_min - y * (up_min - up_max)
C_next = C * g
```

The default `g` range is `1.00` through `1.04`: dirty high load holds the rate,
while clean high load can add up to 4% per eligible update.

### Low or idle load

Once per decay refractory interval, the current rate returns gradually toward
the baseline:

```text
C_next = max(0.99 * C, B)  if C > B
C_next = min(1.01 * C, B)  if C < B
```

Finally every update is clamped:

```text
C_applied = clamp(C_next, Rmin, E)
```

If direction adjustment is disabled, the daemon still calculates and reports
state but does not issue the `tc qdisc change ... cake bandwidth ...` command.

## Bounded adaptive ceiling

The outer controller starts with:

```text
E = S = M
F = none
```

It never permits `E > A` and never rewrites UCI. A direction becomes eligible
for a ceiling probe only when all of the following hold:

- rate adjustment is enabled;
- load is high;
- no bufferbloat is confirmed;
- delay offence count is below the detector threshold;
- average delay is no greater than `Tup`; and
- the fast shaper is at least 98% of `E`.

Eligibility must remain clean for `adaptive_ceiling_hold_time_s`. Without a
known failed bound, the next target is:

```text
P_open = S * (1 + growth_percent / 100)
P = min(P_open, A)
```

When a failed bound exists above the safe bound, the next probe is bounded by
the midpoint:

```text
P = min(P_open, S + (F - S) / 2, A)
```

Probing stops when `F - S` is no more than
`max(0.005 * S, 1000 kbit/s)`. This avoids repeatedly touching an already
localized bottleneck.

The state sequence is:

```text
cruise -> qualify -> probe_ramp -> probe_observe -> backoff -> cruise
```

- `probe_ramp` waits for the fast shaper to reach 98% of `P`.
- A clean `probe_observe` lasting `probe_duration_s` promotes `P` to the new
  safe bound: `S = E = P`.
- Confirmed bufferbloat records the lowest failed target and immediately
  restores `E = S`.
- Loss of eligibility receives a short response-deadline grace period. If it
  persists, the probe is aborted to `S` without falsely recording `P` as bad.
- A global probe-response gap also aborts without poisoning `F`.
- `F` expires after `failed_bound_ttl_s`, allowing a recovered variable link
  to be explored again.
- A stall or daemon restart resets learned bounds to `M`.

### Example

For a symmetric physical link with configured maxima `900000/860000`, caps
`950000/950000`, and 3% probes, clean completed probes would request:

```text
DL: 900000 -> 927000 -> 950000 (cap)
UL: 860000 -> 885800 -> 912374 -> 939745 -> 950000 (cap)
```

These are ceiling targets, not unconditional rate jumps. Each direction still
needs continuous clean high load, must ramp through the fast controller, must
complete its observation interval, and can independently roll back.

## Bounded RAM-only graph history

Graph storage is optional controller telemetry, never a control dependency. Let
`V` be Linux `MemAvailable` plus bytes already owned by all history files. The
safe global maximum is selected from proportional tiers:

| `V` | Safe maximum |
|---:|---:|
| below 64 MiB | 256 KiB |
| 64–128 MiB | 1 MiB |
| 128–256 MiB | 2 MiB |
| 256–512 MiB | 8 MiB |
| 512–768 MiB | 16 MiB |
| 768 MiB–1 GiB | 32 MiB |
| at least 1 GiB | 100 MiB |

Adding owned history back to `V` prevents the safe tier from oscillating merely
because this application consumed its own allowance. In `auto` mode the
requested preset is the largest supported value no greater than one quarter of
the safe maximum. For a manual request `U`, total RAM `T`, and current owned
history `H`:

```text
reserve       = max(32 MiB, 0.05 * T)
pressure_cap  = max(H + MemAvailable - reserve, 0)
effective     = min(U, safe_max(V), pressure_cap, 100 MiB)
per_instance  = floor(effective / enabled_history_instances)
```

When `MemAvailable < 16 MiB`, `effective = 0`; existing graph files are removed
and sampling pauses, while autorate continues normally. The daemon refreshes
the budget every 30 seconds. On reaching an instance cap it streams the newest
rows into a replacement file targeting 75% of the cap, so compaction itself
does not load a large history into RAM. Browser reads are separately paged and
bounded to 10,000 rows.

Traffic-axis autoscaling uses observed DL/UL samples only. The safety floors are
excluded unless `Show safety floors` is enabled, preventing a high configured
floor from visually flattening low ordinary traffic. The fixed scale overlays
sit outside the horizontal scroll track. Follow-to-latest uses the browser's
actual viewport width including a stable scrollbar gutter, so the right edge is
not left one gutter short.

## Choosing fixed SQM or autorate

- A stable fixed-capacity link normally needs CAKE/SQM but may use fixed rates;
  adaptive control adds little when available capacity does not move.
- Variable LTE, cable, radio, overloaded-provider, or failover paths benefit
  from the fast controller because the safe rate changes over time.
- Bounded adaptive ceiling is useful when the configured maximum is a safe
  starting point rather than a known physical hard maximum. The absolute cap
  must still reflect a credible line limit.
- Minimum rate is a hard floor. Set it low enough to remain bufferbloat-free in
  the worst expected condition; the algorithm cannot protect latency below
  that floor.

See [ADAPTIVE_CEILING.md](ADAPTIVE_CEILING.md) for the compact state-machine
contract and [TESTING.md](TESTING.md) for measured behavior.
