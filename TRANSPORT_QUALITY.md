# Transport RTT, detected quality, and optional control

RC8 separated three things that earlier releases mixed together; RC9 keeps
that measurement contract and fixes load/episode detection for passive routed
traffic:

1. a route-bound network RTT measurement;
2. an observational A+/A/B/C/D/F connection rating; and
3. optional CAKE-rate control from confirmed transport delay.

Measurement is enabled with `transport_latency_enabled`. The detected rating
is then always observational. Rate control requires the separate
`transport_controller_enabled=1` opt-in and is disabled by default, including
after an upgrade from RC7. Samples and state live in RAM and never in flash.

## Why RC8 replaced the RC7 measurement

RC7 timed an external `uclient-fetch` process. Its number included process
startup, DNS lookup, TCP connect, TLS handshake, and remote HTTP work. On one
real path that produced a roughly 230-390 ms "idle RTT" while ICMP was
7-25 ms and a browser LibreQoS test reported A+. It was not a network RTT and
could also feed an unsafe control decision.

RC8 removes that value from both rating and control. Status identifies the new
contract as:

```text
transport_probe_method=network_rtt_v3
quality_grade_method=transport_rtt_p90_loaded_minus_p5_idle_v3
```

The legacy process-timed mode remains selectable only for diagnosis. It is
marked untrusted and is rejected by the controller.

## Probe backends

All native backends resolve DNS before timing and create route-bound sockets.
The binding can include the selected L3 device, source IPv4, and mwan3 fwmark.
Route identity is checked again before the result is accepted.

| Backend | What is timed | Persistence | Control trust |
|---|---|---:|---:|
| `websocket` (recommended) | WebSocket ping to matching pong, minus reported server processing | One warmed TLS/WebSocket session per route | trusted |
| `tcp` | TCP connect RTT only | New connection for each raw observation | trusted |
| `http` | Request/response on a warmed HTTP/1.1 TLS session, minus `Server-Timing` when supplied | Reused keep-alive session | trusted |
| `legacy-http` | Whole `uclient-fetch` process | none | untrusted, diagnostic only |

The default endpoint is the same persistent RTT service used by the live
LibreQoS test:

```text
wss://ping-bufferbloat.libreqos.com/ws
```

The WebSocket and persistent-HTTP backends perform an unscored warm-up, so DNS,
TCP/TLS, and protocol handshakes are excluded. A normal measurement contains
four sequential raw RTT observations. The displayed batch value is a robust
median; the controller and rating consume the accepted raw RTTs rather than
duplicating that median four times. A broken persistent connection is discarded
and recreated on the next probe.

`transport_probe_backend`, trust, connection reuse, raw/discarded counts,
server-processing time, and the last rejection reason are exposed in Status.

## Sample acceptance

A native result is accepted only when all of these remain true:

- the configured uplink route identity is unchanged;
- member, device, source address, fwmark, and table still match;
- the backend is trusted and returns non-empty positive RTT data;
- total router CPU is not above `transport_cpu_max_percent` (85% by default);
- the independent rating load phase did not change while the batch ran; and
- the observation still belongs to the same route-bound rating phase when the
  batch completes.

Rejected evidence is reported but never treated as bufferbloat. A route or
source/external-address change clears only that uplink's learned windows.

## RC9 passive rating load detector

The controller's `high_load_thr` answers a different question: whether the fast
rate controller should grow or reduce CAKE now. RC9 therefore does not reuse it
to label rating samples. For direction `d`, achieved rate `R_d`, current CAKE
rate `C_d`, and the samples in the last `W` seconds:

```text
ratio_d(i) = clamp(R_d(i) / C_d(i), 0, 10)
smooth_d   = mean(ratio_d(i), i inside W)
rate_d     = mean(R_d(i), i inside W)
```

Packaged defaults are `W=2 s`, enter ratio `0.60`, exit ratio `0.40`, minimum
rate `2000 kbit/s`, one-second candidate hold, and 1.5-second dropout grace. A
direction enters a loaded phase only when both its smoothed ratio and absolute
rate pass the enter thresholds. When both pass, a 1.5:1 dominance ratio selects
DL or UL; otherwise the sample is `BIDIRECTIONAL`. Leaving a loaded phase uses
the lower exit threshold and half the minimum rate, then requires the dropout
grace. This hysteresis prevents short counter bursts, direction flips, and
small gaps inside one speed-test phase from fragmenting an episode.

The detector consumes the same per-interface RX/TX deltas already calculated
by the daemon exactly once; no WAN aggregate is added to an IFB/device counter.
It runs in `passive` mode continuously, so a sequential browser or CLI test from
a LAN client can produce a rating without clicking anything in LuCI.

## Detected LibreQoS-compatible rating

For endpoint `e` and direction `d`:

```text
idle_baseline(e) = p5(last 120 accepted idle RTT samples for e)
loaded_rtt(d)    = p90(last 40 accepted loaded RTT samples for d)
raw_delta(d)     = loaded_rtt(d) - idle_baseline(e)
scored_delta(d)  = 0, when abs(raw_delta(d)) < 2 ms
                   max(raw_delta(d), 0), otherwise
overall_grade    = worse(grade(download), grade(upload))
```

At least 20 idle samples are required before `BASELINE READY`, and at least 20
loaded samples are required for each scored direction. Download and upload use
separate windows; download evidence can never lower the upload shaper or vice
versa. Adjacent directional phases belong to one test episode when less than
30 seconds apart by default. Bidirectional observations remain diagnostic and do not
lower the overall grade.

| Loaded RTT increase | Grade |
|---:|:---|
| less than 5 ms | A+ |
| less than 30 ms | A |
| less than 60 ms | B |
| less than 200 ms | C |
| less than 400 ms | D |
| 400 ms or more | F |

One-direction evidence is shown as `PARTIAL`, never as a final connection
grade. `CURRENT` shows the active/latest episode and `PREVIOUS` retains the
last completed episode while a new one is collecting. Results record p5, p90,
sample counts, direction, completion time, endpoint, and route identity. A
route change marks retained results stale rather than combining old and new
paths.

The compatibility reference is the live
[LibreQoS Internet Quality Test](https://test.libreqos.com/advanced/) and its
published browser implementation. RC9 measures natural routed traffic instead
of generating the browser test's saturation load, so its Status rating is a
compatible detector, not a claim that an official browser test was run.

## `Get rating` helper

Status offers `Get rating` after the instance, managed SQM, trusted transport
backend, active route, and 20-sample idle baseline are ready. `Automatic`
invokes the existing route-bound speed-test backend in shaped mode and stops as
soon as both directions have enough evidence, with a maximum of three passes.
`Guided client capture` waits while the user runs sequential download and
upload load through the router. Both modes arm a RAM-only bounded marker which
lets the same detector learn a conservative threshold from that test's peak:

```text
capture_enter = min(configured_enter, max(0.15, 0.55 * learned_peak))
capture_exit  = min(configured_exit,  max(0.10, 0.67 * capture_enter))
```

This helps variable links reach a stable phase without weakening normal
passive thresholds. The helper never disables SQM or autorate, never applies a
speed-test result to CAKE, uses the existing per-interface heavy-job lock, and
removes its marker on completion, cancellation, error, or timeout. LuCI reports
baseline, DL/UL counts, phase, smoothed load, finalization time, and the last
rejection reason while it runs.

## Optional strict controller

With `transport_controller_enabled=0`, transport evidence cannot change CAKE
rates, the throughput guard is inactive, and adaptive-ceiling growth is not
blocked by missing transport data. This is the RC8 safe default.

When explicitly enabled, the controller uses the same p5 idle and p90 loaded
statistics, but maintains independent download and upload trackers. A loaded
window above `quality_target_delay_ms` must be confirmed in two consecutive
windows before it may request a reduction. Missing, stale, rejected, or still
learning evidence never fabricates delay and never cuts a rate.

For target `T`, confirmed directional transport increase `D`, and current CAKE
rate `C`, a bounded search candidate is:

```text
factor    = clamp(sqrt(T / D), 0.70, 0.97)
candidate = max(throughput_floor, C * factor)
```

The normal ICMP/OWD loop continues independently. Once transport evidence is
confirmed, the strict effective signal is:

```text
effective_delta_ms = max(ICMP_DL_delta_ms,
                         ICMP_UL_delta_ms,
                         confirmed_transport_delta_ms)
```

The default search observes a candidate for six seconds and permits at most
three steps. It rolls back when the candidate does not materially improve the
starting delay. Reaching the safety floor or step limit sets `quality_limited`
and starts the configured cooldown.

## Throughput safety floor

The floor exists only when both transport control and throughput protection
are enabled. With capacity history:

```text
reference = max(observed_p20, 0.75 * observed_p50)
floor = max(configured_min, absolute_user_floor,
            retention_percent / 100 * reference)
```

Without a Full Auto-Tune reference, `reference = 0.75 * configured_base`. The
default 80% retention therefore preserves 60% of base. No transport search may
cross this per-direction floor or the configured absolute caps.

## Adaptive ceiling and scheduled Auto-Tune

When transport control is enabled, a bounded adaptive-ceiling probe may
promote only while confirmed transport evidence is fresh and clean. Confirmed
delay above target rolls the probe back. With transport control disabled, the
ordinary ICMP/OWD adaptive-ceiling rules remain unchanged.

Periodic Full Auto-Tune is a separate opt-in facility. It keeps its own
maintenance window, quiet-time, budget, validation, and review/auto-apply
policy. Failure, timeout, route mismatch, CPU saturation, or insufficient
throughput retention leaves UCI unchanged.

## Multi-WAN isolation

The native socket receives `SO_BINDTODEVICE`, source-address binding, and
`SO_MARK` where supplied by the resolved route. DNS is outside the RTT clock;
the route identity is verified before and after a probe. Each instance owns a
separate persistent connection, idle baseline, loaded windows, detected grade,
controller state, and throughput reference. Nothing is copied between WANs,
even when both exit through the same public NAT. See [MULTIWAN.md](MULTIWAN.md)
for the complete lifecycle.
