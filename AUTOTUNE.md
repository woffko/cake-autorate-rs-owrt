# Full Auto-Tune

Full Auto-Tune is an experimental creation path for a CAKE Autorate instance.
The existing manual three-step wizard remains available. The automatic path
measures the selected link, calculates a complete balanced proposal, displays
the evidence and parameters, and writes UCI only after the user confirms the
Review step.

Full Auto-Tune also seeds transport-aware runtime control: observed-low and
median throughput become P20/P50 capacity references, the 80% throughput guard
is enabled, and HTTP/TCP latency monitoring is enabled for the created
instance.

## Safety contract

- The job state and raw measurements live under
  `/tmp/cake-autorate-autotune/<job>/`; they never write router flash.
- The browser starts and polls a router-side process. Closing LuCI does not
  strand an rpcd request or partially write a configuration.
- `cancel` terminates the complete job process group. The existing speed-test
  helper owns temporary SQM/autorate suspension and restores it from its signal
  trap.
- Preflight requires a resolved, up interface, a global IPv4 address, and a
  validated route identity. This may be the active main/default WAN or a
  specific online nftables mwan3 member whose L3 device matches the target.
- The test's own reported bytes/rate are authoritative. Aggregate WAN counters
  are deliberately not used for throughput because unrelated clients may be
  using the link.
- A proposal is data, not an applied configuration. The current first stage
  sets `configuration_written=false`; LuCI writes it only after Review.
- Shaped validation temporarily creates a CAKE upload qdisc and download IFB at
  the proposed base rates. It refuses an interface owned by another enabled
  autorate instance or an unknown unmanaged qdisc. An existing sqm-scripts
  queue is stopped and restored through `/usr/lib/sqm/run.sh`.
- A per-interface lock prevents overlapping heavy jobs. Only the selected
  instance and selected SQM queue are paused; other WAN instances continue.

The job runs a shaped test on the same server, independently samples ICMP
latency/loss, TCP/HTTPS latency to Cloudflare, and total CPU, rechecks the WAN
address/route, scores the result, and rolls back before Review. The second
latency signal is deliberate: mobile carriers may prioritize ICMP while TCP is
still badly queued. A borderline candidate gets exactly one bounded base-rate
correction and retest. Full Auto-Tune remains experimental until the real
x86_64 and ARM router gates pass.

## Job phases

1. `preflight`: check helpers (including `uclient-fetch`), backend, interface,
   address, structured `main`/`mwan3` route identity, and link encapsulation.
2. `reflectors`: run the existing reflector planner and keep its recommended
   method and active/spare set.
3. `baseline`: collect individual RTT samples from up to three selected
   reflectors plus five small TCP/HTTPS requests and calculate both baselines.
4. `throughput`: run two unshaped download/upload samples. `speedtest-go`
   validates the first good automatically selected server. Its ID is passed as
   a job-local hard pin to the second raw run and every shaped run.
5. `proposal`: call the Rust daemon's pure `--autotune-proposal` mode.
6. `shaped`: temporarily apply CAKE at proposed base rates, run the same test
   server with SQM bypass disabled, and concurrently collect RTT/loss,
   TCP/HTTPS request latency, and CPU.
7. `correction` (only after a failed score): reduce base rates for high
   latency/loss/CPU, or raise them within observed-low bounds for clean but
   underperforming throughput, then repeat shaped validation once.
8. `review`: return the raw runs, validation, baseline, reflector plan, detected link, and
   apply-ready proposal to LuCI.

The RPC-facing helper supports:

```text
/usr/libexec/cake-autorate-rs/autotune JOB INTERFACE start [BACKEND]
/usr/libexec/cake-autorate-rs/autotune JOB INTERFACE status [BACKEND]
/usr/libexec/cake-autorate-rs/autotune JOB INTERFACE cancel [BACKEND]
```

## Balanced proposal mathematics

Invalid, zero, NaN, and infinite throughput samples are rejected. For each
direction the samples are sorted. With up to three samples, the observed low is
the minimum; with more samples it is p20. The reported centre is the median and
the observed high is p90 using linear interpolation.

For direction `d`:

```text
variability_d = (high_d - low_d) / max(median_d, 1)
variable      = variability_DL >= 0.15 or variability_UL >= 0.15
```

Stable direction proposal:

```text
minimum = 0.70 * low
base    = 0.88 * low
maximum = 0.95 * high
cap     = 1.05 * high
```

Variable direction proposal:

```text
minimum = 0.40 * low
base    = 0.85 * low
maximum = 1.25 * high
cap     = 1.80 * high
```

Rates are rounded to 100 kbit/s and then constrained to
`minimum <= base <= maximum <= cap`. The deliberately wide variable maximum is
not the starting shaper: the shaper starts at `base`, while `maximum` and the
absolute cap leave bounded room for the inner controller and adaptive-ceiling
probes on a recovering radio link.

Activity detection is one tenth of the weaker observed-low direction, rounded
to 100 kbit/s and clamped to 500..20000 kbit/s. This keeps low-rate uploads
visible without treating tiny background traffic as an active connection.

For idle RTT median `m` and p95 `p`:

```text
jitter        = max(p - m, 0)
adjust-up     = ceil(clamp(1.5 * jitter, 3, 15)) ms
delay         = max(adjust-up + 8, 15) ms
adjust-down   = max(delay + 25, 40) ms
```

When either direction is variable, bounded adaptive ceiling is proposed with a
15 second qualification, 3% open step, 8 second observation, 45 second
cooldown, and 900 second failed-bound memory. Stable measurements use a 20/3/8
/60/1800 policy but leave adaptive ceiling disabled.

Detected PPPoE uses Ethernet framing, overhead 44, and MPU 84. Plain Ethernet
uses overhead 18 and MPU 64. Cellular links use raw/no-overhead defaults;
unknown encapsulation produces a Review warning rather than guessing.

## Confidence

Confidence is intentionally simple and visible:

- up to 60 points for three valid samples in both directions;
- 25 points for at least five valid idle RTT samples;
- 15 points for detected encapsulation.

The score is capped at 100. Missing baseline, a single throughput sample,
variable capacity, or unknown link layer also emits a human-readable warning.
Confidence is not a substitute for the separate shaped validation score.

## Shaped validation score

Static validation deliberately compares shaped throughput with the observed
low sample, not the raw median. On a variable link, `base` is designed around
the weakest observation; comparing it with a transient high would incorrectly
mark a safe proposal as a failure.

The score starts at 100. Each direction below 90% retention loses 1.5 points
per percentage point. The larger of ICMP p95 growth and TCP/HTTPS p95 growth
loses 0.5 point per millisecond above 40 ms; loss above 1% loses 5 points per
percentage point; CPU above 80% loses 2 points per percentage point. A
candidate is accepted only when both directions retain at least 80% of
observed-low throughput, both latency signals grow by at most 100 ms, loss is
at most 5%, and CPU is at most 95%. CPU above 80% still reduces the score; 95%
is only the hard failure boundary because the router-local speed-test process
itself consumes CPU that ordinary forwarded client traffic does not.

At least five independent loaded-latency samples are required. Validation uses
one probe per reflector per second and the median per-reflector loss, which
prevents one public DNS service's ICMP rate limiter from masquerading as link
loss. It accepts either explicit fping summaries or complete reply/timeout
lines. At least three TCP/HTTPS loaded samples are also required. Main-route
jobs require the target to be the active default device. Structured Multi-WAN
jobs run fping, `uclient-fetch`, and the selected speed-test backend through
direct `mwan3 use <member> exec ...` argv routing. Every phase compares member,
L3 device, source IPv4, fwmark, routing table, external IPv4, and speed-test
server with the first accepted identity. Missing telemetry or any change fails
closed and leaves UCI untouched.

The sole correction scales both base rates by 0.95 for excessive latency,
loss, CPU, or a mixed failure, or by 1.05 for clean latency with less than 80%
throughput retention. These bounds keep a variable-link base (initially 85% of
observed-low) above the 80% acceptance floor after a downward correction. The
Rust calculator clamps the revised base to
`minimum..min(0.95 * observed_low, maximum)`. A second failed score terminates
the job; there is no unbounded search loop.

## Tests

Rust unit tests cover stable fibre, variable cellular, asymmetric directions,
invalid samples, JSON output, and rate-order invariants. The shell lifecycle
test uses isolated mock helpers to verify progress/result output, shaped score,
job-local server pinning, RAM-only state, process-group cancellation, and both
speed-test and temporary shaper cleanup traps. Real-router acceptance also
checks per-member route identity and that the unselected autorate/SQM instance
continues running.

## Optional scheduler

`cake-autorate-autotune` is a lightweight procd service. Per-instance
`scheduled_autotune_*` options select interval, local hour window, required
quiet time, RAM-only daily traffic budget, and whether a validated result is
automatically applied. The feature defaults off, and auto-apply defaults off.
The scheduler reuses the exact preflight, route identity, two raw samples,
same-server shaped validation, single correction, cleanup, and fail-closed
result described above. See [TRANSPORT_QUALITY.md](TRANSPORT_QUALITY.md) for
the runtime safeguards and [MULTIWAN.md](MULTIWAN.md) for routing behavior.
