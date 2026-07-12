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
