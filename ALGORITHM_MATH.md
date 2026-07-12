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

## Achieved rate and load

The daemon samples Linux interface byte counters. Over an elapsed interval
`dt` seconds:

```text
R = 8 * max(bytes_now - bytes_previous, 0) / (1000 * dt)
L = 100 * R / C
```

Download uses the configured RX counter and upload uses TX. Counter rollback
is saturated at zero. An interval is never treated as shorter than 1 ms.

Load is classified as:

```text
high  if L > 100 * high_load_thr
low   if not high and R > connection_active_thr_kbps
idle  otherwise
```

The packaged `high_load_thr` default is `0.75`, or 75% of the current CAKE
rate.

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
