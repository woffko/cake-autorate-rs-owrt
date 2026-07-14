# Transport-aware quality control

The optional transport-aware controller complements the normal ICMP/OWD loop
with small HTTP/TCP requests. It addresses links where ICMP remains clean while
ordinary TCP traffic queues badly. It is disabled by default and does not write
samples to flash.

## Strict controller signal

Each configured HTTP(S) endpoint keeps its own 20-sample idle-latency window.
The endpoint baseline is the 20th percentile of that window. A sample taken
while either direction is above `high_load_thr` produces:

```text
transport_delta = max(loaded_request_ms - endpoint_idle_p20_ms, 0)
```

Two loaded samples are required before the signal is confirmed; the controller
uses the median of the newest four. A stale, failed, or still-learning signal
never causes a rate reduction. It only blocks high-load growth and adaptive
ceiling promotion until clean transport evidence returns. ICMP continues to
run independently, and the value exposed to Status and Graphs is:

```text
effective_delta_ms = max(ICMP_DL_delta_ms,
                         ICMP_UL_delta_ms,
                         confirmed_transport_delta_ms)
```

The strict controller class uses the effective confirmed signal above. Its
threshold labels match LibreQoS grade boundaries, but its sampling deliberately
remains conservative and stateful for shaper control:

| Effective loaded increase | Controller class |
|---:|:---|
| less than 5 ms | A+ |
| less than 30 ms | A |
| less than 60 ms | B |
| less than 200 ms | C |
| less than 400 ms | D |
| 400 ms or more | F |

Before enough endpoint and loaded samples exist this signal is `LEARNING` or
`BASELINE READY`. It continues to drive the throughput floor and bounded search
described below. It is not the user-facing connection test and is never
weakened merely to make a displayed grade look better.

## Detected LibreQoS-like rating

RC7 adds a second, observational tracker for the Status and Graphs pages. Its
statistics follow the method used by the live LibreQoS Internet Quality Test in
July 2026:

```text
idle_baseline(endpoint) = p5(idle HTTP/TCP samples for that endpoint)
loaded_delta(direction) = p90(loaded HTTP/TCP samples for that direction)
                          - idle_baseline(the selected endpoint)
scored_delta(direction) = 0, when abs(loaded_delta) < 2 ms
                          max(loaded_delta, 0), otherwise
overall_grade = worse(grade(download), grade(upload))
```

The bidirectional phase is retained as a diagnostic but, matching the live
test, does not affect the overall grade. The same exact `<5`, `<30`, `<60`,
`<200`, `<400`, otherwise-F boundaries are used. This is described as
*LibreQoS-like* rather than an official LibreQoS result because the daemon uses
natural routed traffic and small rotating HTTP probes rather than LibreQoS's
browser traffic generator.

Each endpoint owns a 40-sample idle window. At least three clean idle samples
are required before its baseline can score a load episode, and at least three
loaded samples are required before that direction is final. One endpoint is
selected for an episode so different DNS/TCP/TLS paths are never subtracted
from one another. During an active episode Status publishes a partial
`CURRENT` value and its sample progress. When the episode ends, the completed
result becomes `PREVIOUS`; while the next episode is still `COLLECTING`, that
previous result remains visible instead of being replaced by an empty grade.

Every result records its endpoint, baseline p5, loaded p90, direction, sample
count, completion time, and route identity. A route, source/external address,
or member change clears the affected endpoint learning and marks the retained
previous result `STALE`. Results and baselines never cross uplinks.

The compatibility reference is the live
[LibreQoS Internet Quality Test](https://test.libreqos.com/advanced/) and its
published browser implementation. LibreQoS may evolve independently; this
document and the daemon status field
`quality_grade_method=p90_loaded_minus_p5_idle` identify the implemented
snapshot explicitly.

## Throughput safety floor

Transport-driven rate search cannot cross a per-direction safety floor. With
capacity history:

```text
reference = max(observed_p20, 0.75 * observed_p50)
floor = max(configured_min, absolute_user_floor,
            retention_percent / 100 * reference)
```

When no Full Auto-Tune history exists, `reference = 0.75 * configured_base`.
The default 80% retention therefore preserves 60% of base. Full Auto-Tune
writes its observed low and median values as the P20/P50 references. The floor
is still bounded by the absolute adaptive cap.

## Bounded natural-traffic search

Confirmed transport delay during natural high load may start a short search.
For target `T`, measured delta `D`, and current CAKE rate `C`:

```text
factor = clamp(sqrt(T / D), 0.70, 0.97)
candidate = max(floor, C * factor)
```

The default policy observes each candidate for six seconds and permits at most
three steps. A candidate must improve latency by at least 10 ms to continue.
Worsening or no meaningful gain stops the search and rolls back to the best
candidate only when it improved the starting delay by at least 30 ms or 25%;
otherwise it restores the starting rate. Reaching the floor or step limit sets
`quality_limited` and starts a 15-minute cooldown. This is the explicit
safe-limit outcome when an A-like target cannot be achieved without destroying
throughput.

The normal fast ICMP controller remains responsible for its existing quick
bufferbloat response. Transport-aware search does not count a missing HTTP
sample as bufferbloat and cannot bypass configured CAKE caps.

## Adaptive ceiling interaction

A bounded adaptive-ceiling probe may qualify or promote only while both the
normal ICMP detector and a fresh confirmed transport signal are clean. A loaded
transport delta above the target makes an in-progress ceiling probe fail and
roll back. This prevents prioritized ICMP from approving a ceiling that is bad
for ordinary traffic.

## Scheduled Full Auto-Tune

Periodic calibration is independently optional and disabled by default. Its
per-instance controls include interval, local maintenance window, required
quiet time, daily traffic budget, and auto-apply. State, last-run time, and byte
accounting live under `/var/run/cake-autorate-autotune-scheduler` and disappear
on reboot.

The scheduler invokes the same fail-closed Full Auto-Tune job used by the
wizard. The default is review-only: a validated proposal is retained in `/tmp`
but UCI is unchanged. Auto-apply must be selected explicitly; even then, UCI is
committed and the service restarted only after shaped validation returns
`complete`. Failure, timeout, a busy link, an unsuitable time window, or an
exhausted daily budget leaves the running configuration unchanged.

## Per-uplink routing and baselines

Main-route instances require the selected device to be the active default WAN.
Structured Multi-WAN instances instead execute the HTTP client directly as
`mwan3 use <member> exec ...`, so `uclient-fetch` follows the same selected
member as ICMP and speed-test traffic without requiring `curl`.

Every transport sample carries the complete uplink route identity. Samples are
discarded if member, L3 device, source address, fwmark, routing table, or
external address no longer matches. Each instance owns independent endpoint
baselines and loaded windows; they are cleared on failover, PPPoE address
change, route change, and offline recovery. No sample or baseline may cross
from one WAN to another. See [MULTIWAN.md](MULTIWAN.md) for lifecycle details.
