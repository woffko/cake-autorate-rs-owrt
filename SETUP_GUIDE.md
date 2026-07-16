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
3. Choose **Full Auto-Tune**. Stop large downloads and uploads first. The
   strict run measures idle ICMP across three independent reflector families,
   persistent native transport latency, two unshaped throughput samples, and a
   shaped validation candidate. It counts forwarded client traffic separately
   during every heavy phase.
4. If background traffic blocks calibration, prefer **Retry when quiet**. The
   explicit **Continue conservatively** action applies to that run only: it
   subtracts measured background with an extra margin, never raises confirmed
   maxima or adaptive caps, and retains a direction whose evidence is not
   usable. The Review page labels such a result **LOW confidence** and it is
   not eligible for scheduled Auto-Apply.
5. Review the proposed min/base/max rates, absolute adaptive-ceiling caps,
   link-layer overhead, latency thresholds, validation gates, and warnings.
   Nothing is written before **Use proposal/Create**, and the staged UCI
   change still requires **Save & Apply**.
6. On **Status**, confirm that the uplink becomes `ACTIVE`, the controller is
   `RUNNING`, CAKE rates are non-zero, and Quality reaches `BASELINE READY`.
   Use **Get rating** while the link is otherwise quiet to obtain a complete
   download/upload grade.
7. Enable **Graphs** only if RAM history is useful. Samples stay in `/var/run`
   and disappear on service stop or reboot. Start with the automatic memory
   budget and a 10-second interval.

## Existing instances

Use **Re-run Auto-Tune** immediately before **Edit** in the Settings row. It
opens the calibration page with the instance interface, route/member, backend,
queue and current limits prefilled. The current configuration remains active
until a passing Review result is explicitly staged and saved. Re-run preserves
the instance's explicit Adaptive Ceiling enabled/disabled choice; changing it
requires the corresponding Review control.

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
contaminated, and infeasible runs remain available as diagnostics but expose no
apply action and do not replace the current UCI configuration. If the user
explicitly selected conservative mode, a passing LOW-confidence result may be
applied manually from Review; it is never eligible for scheduled Auto-Apply.

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
