# Transport-aware quality control

The optional transport-aware controller complements the normal ICMP/OWD loop
with small HTTP/TCP requests. It addresses links where ICMP remains clean while
ordinary TCP traffic queues badly. It is disabled by default and does not write
samples to flash.

## Signals and classification

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

The Status grade is explicitly an estimate, not a LibreQoS or Ookla result:

| Effective loaded increase | Estimated class |
|---:|:---|
| up to 5 ms | A+ |
| up to 30 ms | A |
| up to 60 ms | B |
| up to 200 ms | C |
| up to 400 ms | D |
| over 400 ms | F |

Before enough endpoint and loaded samples exist the class is `LEARNING`.
Status also shows confidence, sample age, per-direction classes, and a reason.

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
