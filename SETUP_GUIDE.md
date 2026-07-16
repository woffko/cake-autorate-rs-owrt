# Quick setup guide

Fresh installations intentionally create no autorate instance and no managed
SQM queue. Open **Network → CAKE Autorate SQM → Settings** and create the first
instance only after identifying the intended uplink.

## Recommended first setup

1. Select **Create instance** and give the link a descriptive name such as
   `wan_sqm` or `wanb_sqm`.
2. Choose the target path shown by LuCI, for example
   `wan — pppoe-wan — eth2`. For a normal single-WAN router use **Main routing
   table**. On nftables mwan3 select the member that resolves to this same L3
   device.
3. Choose **Full Auto-Tune**, then select a calibration profile:

   - **Best overall** is recommended for most links. It targets an A-like
     loaded-delay result while retaining at least 80% of observed-low
     capacity.
   - **Gaming** targets A+ with a 70% throughput floor and configures CAKE
     `diffserv4`. It prioritizes only traffic already marked with DSCP; it does
     not detect games or rewrite client policy. Use it only when markings are
     trusted.
   - **Fair** keeps at least 90% of observed-low capacity and aims for C or
     better (no more than 200 ms effective loaded-delay growth), favoring
     sustained large downloads/uploads over the strictest latency.

   Existing instances without a saved profile default to Best overall.
4. Stop large downloads and uploads first. The
   strict run measures idle ICMP across three independent reflector families,
   persistent native transport latency, two unshaped throughput samples, and a
   shaped validation candidate. It counts forwarded client traffic separately
   during every heavy phase.
5. If background traffic blocks calibration, prefer **Retry when quiet**. The
   explicit **Continue conservatively** action applies to that run only: it
   subtracts measured background with an extra margin, never raises confirmed
   maxima or adaptive caps, and retains a direction whose evidence is not
   usable. The Review page labels such a result **LOW confidence** and keeps it
   diagnostic-only: it cannot be applied or consumed by scheduled Auto-Apply.
6. Review the selected profile, proposed min/base/max rates, absolute
   adaptive-ceiling caps, link-layer overhead, latency thresholds, validation
   gates, exact CAKE class policy, and warnings. Nothing is written before
   **Use proposal/Create**, and the staged UCI change still requires
   **Save & Apply**.
7. On **Status**, confirm that the uplink becomes `ACTIVE`, the controller is
   `RUNNING`, CAKE rates are non-zero, and Quality reaches `BASELINE READY`.
   Use **Get rating** while the link is otherwise quiet to obtain a complete
   download/upload grade.
8. Enable **Graphs** only if RAM history is useful. Samples stay in `/var/run`
   and disappear on service stop or reboot. Start with the automatic memory
   budget and a 10-second interval.

## Existing instances

Use **Re-run Auto-Tune** immediately before **Edit** in the Settings row. It
opens the calibration page with the instance interface, route/member, backend,
queue, current limits and saved profile prefilled. The current configuration
remains active until an eligible Review result is explicitly staged and saved.
Changing profile affects the complete proposal and SQM policy, not just the
grade label. Re-run preserves the instance's explicit Adaptive Ceiling
enabled/disabled choice; changing it requires the corresponding Review
control.

## Reading Auto-Tune diagnostics

The Review/diagnostics page keeps three throughput percentages separate for
download and upload:

| Field | Meaning |
|---|---|
| Candidate realization | Achieved throughput divided by the temporary CAKE candidate; low values request a repeat of the same measurement |
| Capacity retention | Achieved throughput divided by the unshaped observed-low capacity; this is the throughput safety gate |
| Candidate capacity | Temporary CAKE candidate divided by observed-low capacity; this shows how conservative the candidate is |

Latency is shown as loaded p95 minus idle p95 for both ICMP and native
transport. The page also reports loss, CPU, forwarded background for each
phase, every pass/fail gate, and any typed correction:
`retry-measurement`, `increase`, `decrease`, `mixed`, or `infeasible`.

`infeasible` means the requested latency/rate constraints cannot be satisfied
without crossing the throughput safety floor or configured bounds. It is not
an instruction to accept a more destructive rate. Failed, incomplete, strictly
contaminated and conservative runs remain diagnostics and do not replace the
current UCI configuration. Gaming and Best overall also expose no action after
an infeasible result.

Fair is the narrow exception: if measurement integrity, route, background,
candidate-realization, 90% retained-capacity and CPU gates all pass but class C
does not, Review offers the best hard-safe shaped candidate as a manual choice.
For an existing managed instance it may additionally offer **Disable autorate
and SQM (comparison suggestion)** after a clean simultaneous bidirectional
no-SQM control proves no worse grade, no material latency benefit from shaping,
and at least 2% more throughput in both directions. The choice is never
preselected, requires explicit confirmation, and scheduled Auto-Apply cannot
use it. **Keep current settings** writes nothing.

Inside **Edit**, **Autorate setup** is split into focused groups:

- **Connection & routing** selects the managed interface and the main or mwan3
  path.
- **Rate limits** contains the minimum, base and maximum rates used by the fast
  controller.
- **Adaptive ceiling** contains the optional growth state machine and its
  absolute caps.
- **Latency probes** configures ICMP reflectors and probe timing.
- **Quality & rating** configures native transport measurement and the detected
  connection grade.
- **Controller** contains the response thresholds and timing of the autorate
  controller itself.

These are navigation groups, not separate configurations. Values in groups
that are not currently visible are still validated and saved with the same UCI
instance. Use **Advanced** only for compatibility and low-level options that do
not belong to the normal setup workflow.

## Multi-WAN rules

- One autorate instance owns one uplink and one CAKE/IFB pair.
- Never point two enabled instances at the same L3 device or SQM queue.
- Each mwan3 member has its own latency baseline, rating, throughput reference,
  adaptive ceiling and Auto-Tune schedule.
- An offline or mismatched member is not a valid calibration route. An online
  standby member may be calibrated through its explicitly selected forced
  `mwan3` member route; route identity must remain stable for the full run.

For the algorithms and safety invariants see [Controller
mathematics](ALGORITHM_MATH.md), [Full Auto-Tune](AUTOTUNE.md), and
[Multi-WAN routing](MULTIWAN.md). Repeatable checks and anonymized observations
are in [Testing](TESTING.md).

## Checking CPU overhead

The CPU column in Status is the total router utilization, so forwarded traffic,
PPPoE, CAKE and network softirq work are intentionally included. To distinguish
that data-plane load from the control application, run:

```sh
/usr/libexec/cake-autorate-rs/cpu-profile 30
```

The report shows router busy/softirq percentages and each autorate daemon,
pinger and Auto-Tune scheduler both as a percentage of one logical CPU and of
the router's total logical-CPU capacity. The helper is read-only, samples for
5–300 seconds, uses only a temporary file under `/tmp`, and does not enable
logging or write to flash.
