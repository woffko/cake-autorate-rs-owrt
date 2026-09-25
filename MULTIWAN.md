# Multi-WAN Routing and Lifecycle

The current implementation treats every autorate instance as the owner of one uplink. The model is
deliberately explicit:

```text
one instance
  -> one logical route/member
  -> one resolved L3 device and source address
  -> one managed CAKE/IFB pair
  -> one profile-specific outbound rule binding
  -> one set of latency, quality, throughput, and ceiling state
```

State learned on one uplink is never reused on another. This matters even when
two members happen to exit through the same public address, as can occur when
one WAN is cascaded through another router.

## Requirements

Structured Multi-WAN mode requires:

- OpenWrt 25.12 with the native nftables `mwan3` backend;
- a configured and enabled IPv4 mwan3 interface/member;
- the member-scoped `ubus call mwan3 status` API;
- a resolved L3 device that matches the instance target interface; and
- a separate SQM queue/device for every enabled autorate instance.

The application consumes existing network and mwan3 configuration. It does not
create gateways, tracking targets, metrics, weights, or policies on behalf of
the user.

## UCI model

The routing fields are:

```uci
config cake_autorate 'wan_sqm'
        option enabled '1'
        option wan_if 'pppoe-wan'
        option route_mode 'mwan3'
        option mwan3_member 'wan'
        option manage_sqm '1'
        option sqm_interface 'pppoe-wan'
        option sqm_section 'cake_wan_sqm'
```

`route_mode` accepts:

- `main`: use the normal main routing table and require the target to be the
  active default route;
- `mwan3`: force all probes and calibration traffic through
  `mwan3_member`; or
- `auto`: use structured mwan3 when a member is set, otherwise use `main`.

The default-device label uses the lowest metric, not route dump order. Equal
best metrics through different devices and unresolved multipath do not identify
one default device. `/proc/net/route` entries must have a zero destination and
zero mask; the lower half of a split default (`0.0.0.0/1`) is not a default entry.
A VPN default on another device therefore does not authorize `main` probes on
the selected WAN. More-specific routes and PBR still need their own route proof:
a matching default label alone is not acceptance of those configurations. Use
the explicit policy-route mode below for PBR and VPN-default setups.

`main` now refuses nonstandard IPv4 policy rules and foreign split-default
overrides before starting probes or calibration. It requires the standard
local/main/default rules; it does not attempt to guess that a custom selector
is irrelevant. Use a supported, explicit route authority for policy routing.
Owned calibration traffic additionally has a postrouting egress guard: packets
resolved to a different device are dropped and make accounting fail, rather
than measuring that device. The guard permits only local loopback DNS as an
exception; it does not attest the resolver daemon's upstream forwarding path.

### Explicit policy route (`route_mode=explicit`, Full only)

The Full controller can pin its latency and transport probes to an existing
policy route: `route_source_ipv4` (assigned to the target interface),
`route_table` (nonzero numeric table), `route_fwmark` and `route_fwmark_mask`
(nonzero mark inside a nonzero mask), and optionally `route_dns_ipv4` for
transport-probe name resolution. The mode never creates or changes VPNs, ip
rules or tables. Before probes start and on every route check the controller
observes the address, the rule list (only the local rule and non-overlapping
mark rules may precede the selected rule) and the table, whose default must
leave through the target interface. Probes run under a reserved per-owner
group with an exact nft mark/egress pin; a removed rule, a table rerouted to
another device or a changed source moves the uplink to `RECHECKING` and stops
probe egress instead of falling back to the main table. A crashed controller's
pin is removed by the next owner only when it carries that exact owner
generation. Lite refuses the mode.

Manual Speed Test and Full Auto-Tune accept the mode when `route_dns_ipv4` is
set. LuCI passes the committed route as launch authority and the daemon checks
it against the configuration. The backend binds to the route source, its DNS
queries go only to that server, and its traffic carries the selected mark
through an owned nft pin with exact accounting; a route that fails attestation
refuses the run instead of using the main table. Without `route_dns_ipv4` these
operations are refused, never retried with the system resolver. Scheduled
calibration reads the same route fields and passes the same launch authority;
an incomplete route or missing `route_dns_ipv4` is reported as a scheduler
configuration issue for that instance and no run starts.
These checks do not implement a general device/PBR mode or IPv6-only support.

For `mwan3`, route discovery requires an unambiguous pair of unconditional
ingress-device/table and mark/table rules. Conflicting tables, marks or masks,
negated/conditional selectors and invalid masks are rejected instead of taking
the first match. Equivalent duplicate rules are accepted. Missing authority
is an inspection error, not an online route inferred from the wrapper's mask.
This pair check does not by itself prove that other policy rules cannot override
traffic; it is not acceptance of arbitrary PBR configurations.

Member and device names are validated as data, never evaluated by a shell.
Automatic pinger argv is derived from the current admitted route snapshot.
Offline/rechecking state and mismatched device/member/source or absent mwan3
mark/table prevent startup. User-specified pins are preserved only when they
agree with that identity; conflict is an error, not an instruction to choose
another WAN. Native Apply and the wizard no longer persist generated pins.
Old `ping_extra_args=-I ...` has no trustworthy provenance marker: review it
after a route change rather than allowing automatic deletion of user flags.

External helpers use a direct argv vector:

```text
mwan3 use <member> exec <program> <arguments...>
```

Native transport RTT sockets do not spawn a route helper. They apply the resolved
L3 device (`SO_BINDTODEVICE`), source IPv4 bind, and mwan3 `SO_MARK` directly;
DNS resolution happens before the RTT clock starts. Persistent connections are
owned by one route identity and discarded when that identity changes.
Rating-load windows, directional sample counters, active episodes, and capture
markers are likewise owned by one instance/route identity; a WAN
cannot complete a grade using another WAN's traffic or baseline.

The legacy exact prefix `ping_prefix_string='mwan3 use <member> exec'` is
migrated by the init script to these structured fields. Arbitrary free-form
prefixes are not accepted for structured Multi-WAN.

## Route identity

An accepted route is represented by:

```text
route_mode | member | L3 device | source IPv4 | fwmark | routing table
```

Runtime Status additionally reports member status, public IPv4, policy share,
and a stable error code/reason. ICMP, native WebSocket/TCP/HTTP RTT,
router-side speed tests, and Full Auto-Tune all verify the same identity. A
result is rejected if the member is offline, the device differs, the source
address changes during a test, the route mark/table no longer matches, a
forced helper does not use the expected member, or the public address changes
between calibration phases.

External address alone is not considered proof of route identity. Device,
source address, fwmark, and table remain authoritative when both WANs are
behind the same upstream NAT.

## Lifecycle

Each instance has an uplink lifecycle independent of its controller state:

| State | Meaning | Controller behavior |
|---|---|---|
| `ACTIVE` | Member is online and selected by the default mwan3 policy | Normal probing and autorate control |
| `STANDBY` | Member is online but has a zero policy share | Forced probes remain admitted and isolated to this member; ordinary idle policy still applies |
| `OFFLINE` | A successful inspection explicitly reports an offline or mismatched route | Stop pingers and freeze adjustment without adding reflector offences |
| `RECHECKING` | Inspection failed or member state is transient/unknown | Revoke route admission while preserving the last learned identity |
| `LEARNING` | Route recovered or changed and its baseline is being rebuilt | Wait for matching identity confirmation, then probe; baseline qualification still gates control |

The daemon derives active/standby from the configured default mwan3 policy,
not merely from the Linux main-table default route. This is important during
policy failover: the physical primary route may still exist while mwan3 has
already assigned 100% of traffic to the backup.

There is no `route_stability_s` timer. A new identity needs two consecutive
matching successful observations before route admission. Errors/transient
member states revoke admission; they do not become OFFLINE by timeout or erase
the last learned identity. Actual identity change or explicit offline evidence
resets only the affected uplink. After confirmation the lifecycle counts
`3 * max(no_pingers, 1)` accepted fresh replies before ACTIVE/STANDBY. The
separate cold-baseline qualification still gates latency-based control.
The learning pinger is kept awake even
when the normal idle policy would otherwise stop it. Only after sufficient
fresh evidence does the lifecycle return to `ACTIVE` or `STANDBY`.

These events invalidate stale learning:

- member offline and later online;
- L3 device change;
- source IPv4 change, including PPPoE reconnect;
- fwmark or routing-table change; and
- a different selected member.

Transport endpoint baselines, loaded samples, detected quality, load-detector
state, and adaptive-ceiling safe/failed bounds are reset together for that
instance. Configured UCI capacity references are not rewritten by this reset.
The last completed detected grade remains visible but is marked
stale until that route learns and completes a new episode. Another uplink
continues independently.

## SQM ownership and calibration isolation

With `manage_sqm=0`, ordinary controller observation checks readable counters and
one addressable root `cake`/`cake_mq` qdisc on each direction whose rate adjustment
is enabled. It does not require another direction's external qdisc to disappear,
nor infer ownership of traffic steering from an `ifb` name. The operator remains
responsible for steering traffic through those explicitly selected queues.
Missing control targets enter `WAITING_EXTERNAL_SQM`; the daemon does not start
SQM, create/delete queues, or rewrite ingress to repair them. When they return,
it resets its own measurements and resumes ordinary bandwidth control.

Bandwidth changes inspect the current root type and nonzero handle and address
that handle explicitly. This matters because a bare `tc qdisc change` can graft
a different qdisc kind in the Linux API. Built-in handle-zero roots and CAKE
leaves below another scheduler are not addressable control targets here.
This extra observation is not proof that arbitrary ingress actions can be restored.

Native operations still require their managed configuration/state authority;
bootstrap Apply also verifies its generated IFB ingress against the stricter
exclusive contract. `ctinfo`, policing, custom chains or multiple filter
rules can be compatible with bandwidth-only control but remain unsupported for
native topology mutation/restore unless fully modeled. The blocker explains this
distinction instead of offering to delete those external actions.

If the native runtime owner's record is unreadable or invalid, ordinary rate
control remains held. Status exposes `runtime_control_degraded`,
`runtime_control_error`, and `runtime_control_held`; Full and Lite show the error
as a visible warning. A successful observation of a known active owner clears
the error but keeps the hold. Only an exact Idle observation can release that
operation hold, and any independent active override still blocks ordinary
control. Repeated errors or elapsed time never authorize resetting a possibly
foreign queue. Diagnose/recover the owning operation before resuming control.

The init script rejects two enabled instances that resolve to the same managed
CAKE device or SQM section. Each valid member receives its own CAKE root qdisc
and download IFB. PPPoE, Ethernet, and a cascaded Ethernet WAN are supported as
long as the selected member's L3 device matches `wan_if`/`sqm_interface`.

A router-side speed test or Full Auto-Tune job:

1. acquires a per-interface lock;
2. records the selected route identity and external address;
3. pauses only the selected autorate daemon;
4. removes or replaces only the selected uplink's SQM during an unshaped or
   temporary shaped phase;
5. runs all traffic through the selected member;
6. validates route, address, and server consistency; and
7. restores that daemon and its previous qdiscs on success, failure, timeout,
   or cancellation.

For `speedtest-go`, Full Auto-Tune selects a server on the first raw sample and
passes that ID to every later raw and shaped phase. The pin is job-local and
does not rewrite UCI. Because `speedtest-go` is a static Go binary, it cannot
use mwan3's `LD_PRELOAD` socket-mark wrapper. The package therefore runs only
that backend as its unprivileged `cake-speedtest` user and installs a temporary
UID-scoped nftables route hook after mwan3's output hook. A global RAM lock
prevents two members from assigning different marks to that UID concurrently.
The result is accepted only when the selected L3 interface's RX/TX counters
also prove a minimum share of the reported download and upload payload. The
temporary table and lock are removed on success, failure, signal, or stale-owner
recovery. A candidate that changes route, address or server, or lacks this byte
proof, fails closed and writes no configuration.

The optional native traffic classifier follows the same isolation rule. It
owns one private nftables table for the application but emits a separate
`oifname` rule set for each instance's resolved L3 device. Two enabled
instances cannot claim the same target. The RAM-only attestation binds
instance, device, active profile, and actual ruleset checksum; a stale profile
or interface is shown as `DRIFTED` rather than silently reused. Classification
changes DSCP only and does not choose a mwan3 route, create a CAKE qdisc, or
change a rate. See [Profile traffic priorities](TRAFFIC_PRIORITIES.md).

## LuCI behavior

The setup view labels logical and physical topology, for example:

```text
wan -> pppoe-wan -> eth2
wanb -> eth0
```

The Multi-WAN creation mode detects unique enabled members and previews the
instance names, target devices, SQM sections, and conflicts before saving.
Full Auto-Tune then presents one member at a time. Every member retains an
independent calibration profile, result, diagnostics, and Accept/Skip decision;
changing the current profile or retrying its test cannot alter an earlier
accepted result. A failed or inconclusive run remains on that member for Retry,
Conservative retry, or explicit Skip instead of silently starting the next
WAN. Apply is enabled only for an exact option whose public Review and private
evidence agree. An option with `auto_apply_evidence_pass=true` and no
acknowledgement codes may be eligible for unattended apply; every option with
listed trade-offs is an explicit per-member decision. Skip is available after settled
runtime recovery, including before testing when the user deliberately wants an
uncalibrated placeholder. Selecting an accepted option starts that member's
independent receipt-backed native transaction: start, watch/reconnect, exact
UCI/service mutation, runtime proof, terminal receipt, then authoritative UCI
reload before the next member. The first failure halts the batch and no
unconfirmed option is written. The final Review summarizes the already
accepted members and pending skips. Explicitly skipped uplinks are added disabled in a final
rollback-enabled transaction only after proving that no native Apply authority
remains. This avoids combining independent CAKE owners into one ambiguous
transaction.
Status shows lifecycle state, route/member/device, source and external address,
fwmark, routing table, policy share, complete Services reconciliation
(daemon/SQM/CAKE/IFB/redirect/rules), and the reason for standby/offline.
Settings exposes **Traffic priorities** separately on every instance row; the
opened view is bound to that uplink and cannot list or edit rules from another
member. The former global priorities tab is intentionally absent.
Graphs remain separate per instance, are stacked vertically, and annotate
route/lifecycle, rating-phase, directional-count, and detected-grade changes.
`Get rating` locks only the selected interface and routes separate shaped
download-only/upload-only automatic phases through that instance's validated
member. Its quiet check, background reference, phase counters, and contamination
decision are per-uplink. History remains RAM-only. One
global memory budget is divided across every enabled instance; no WAN can claim
another WAN's full allowance, and low-memory suspension affects only telemetry,
not shaping or route lifecycle.

When the creation wizard calibrates an uplink with no saved rates, its traffic
threshold is not derived from a synthetic fallback. The member's first
baseline is retained provisionally, the first speed control remains pinned to
that member, and the measured DL/UL capacities resolve separate total-interface
and forwarded-client limits. A retrospectively contaminated member remains on
its own Review step and may be retried, continued once conservatively, or
skipped; it cannot contaminate or advance another member. Any safe option
retaining reviewable contamination is manual-only and carries the exact
acknowledgement codes; a fresh fully clean rerun may remove them.

## Diagnostics

Useful checks are:

```sh
/usr/sbin/cake-autorated --mwan3-info
ubus call mwan3 status '{"interface":"wan"}'
mwan3 use wan exec env
cat /var/run/cake-autorate/wan_sqm/status.json
tc qdisc show dev pppoe-wan
tc qdisc show dev ifb4pppoe-wan
```

The exported diagnostic bundle includes redacted topology, route identity,
mwan3 state, qdisc state, and runtime status. It omits credentials and never
exports arbitrary command prefixes.

## Current boundary

Structured routing and public-address validation currently use IPv4. IPv6
mwan3 members may coexist in the router configuration, but IPv6-only autorate
calibration is not yet a supported RC path. When `main` finds an IPv6 address
but no global IPv4 source, it reports `unsupported_family` with an IPv6-only
capability explanation and keeps probes disabled. No IPv6 worker, counter,
recovery or route-identity support is implied by this diagnosis.
Load balancing can mark multiple
members active; each instance still remains bound to its configured member and
must own a distinct shaper.
