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
   strict run measures idle ICMP and native transport latency, two unshaped
   throughput samples, and a shaped validation candidate.
4. If background traffic blocks calibration, prefer **Retry when quiet**. The
   explicit **Continue conservatively** action applies to that run only: it
   subtracts measured background with an extra margin, never raises confirmed
   maxima or adaptive caps, and retains a direction whose evidence is not
   usable. The Review page labels such a result **LOW confidence**.
5. Review the proposed min/base/max rates, absolute adaptive-ceiling caps,
   link-layer overhead, latency thresholds and warnings. Nothing is written
   before **Use proposal/Create**, and the staged UCI change still requires
   **Save & Apply**.
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
until the Review result is explicitly staged and saved.

## Multi-WAN rules

- One autorate instance owns one uplink and one CAKE/IFB pair.
- Never point two enabled instances at the same L3 device or SQM queue.
- Each mwan3 member has its own latency baseline, rating, throughput reference,
  adaptive ceiling and Auto-Tune schedule.
- A standby/offline member is not a valid calibration route. Wait for it to be
  active, or test it through the corresponding member after failover.

For the algorithms and safety invariants see [Controller
mathematics](ALGORITHM_MATH.md), [Full Auto-Tune](AUTOTUNE.md), and
[Multi-WAN routing](MULTIWAN.md). Repeatable checks and anonymized observations
are in [Testing](TESTING.md).
