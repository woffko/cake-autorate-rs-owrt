# Multi-WAN Routing and Lifecycle

RC6 treats every autorate instance as the owner of one uplink. The model is
deliberately explicit:

```text
one instance
  -> one logical route/member
  -> one resolved L3 device and source address
  -> one managed CAKE/IFB pair
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

Member and device names are validated as data, never evaluated by a shell. The
executed form is a direct argv vector:

```text
mwan3 use <member> exec <program> <arguments...>
```

The legacy exact prefix `ping_prefix_string='mwan3 use <member> exec'` is
migrated by the init script to these structured fields. Arbitrary free-form
prefixes are not accepted for structured Multi-WAN.

## Route identity

An accepted route is represented by:

```text
route_mode | member | L3 device | source IPv4 | fwmark | routing table
```

Runtime Status additionally reports member status, public IPv4, policy share,
and a stable error code/reason. ICMP, HTTP/TCP, router-side speed tests, and
Full Auto-Tune all verify the same identity. A result is rejected if the
member is offline, the device differs, the source address changes during a
test, the forced command does not use the expected member, or the public
address changes between calibration phases.

External address alone is not considered proof of route identity. Device,
source address, fwmark, and table remain authoritative when both WANs are
behind the same upstream NAT.

## Lifecycle

Each instance has an uplink lifecycle independent of its controller state:

| State | Meaning | Controller behavior |
|---|---|---|
| `ACTIVE` | Member is online and selected by the default mwan3 policy | Normal probing and autorate control |
| `STANDBY` | Member is online but has a zero policy share | Keep isolated state, stop unnecessary active probing |
| `OFFLINE` | Interface/member/route identity is unavailable or mismatched | Stop pingers and freeze adjustment without adding reflector offences |
| `LEARNING` | Route recovered or changed and its baseline is being rebuilt | Probe the recovered path but block ceiling growth until stable |

The daemon derives active/standby from the configured default mwan3 policy,
not merely from the Linux main-table default route. This is important during
policy failover: the physical primary route may still exist while mwan3 has
already assigned 100% of traffic to the backup.

Recovery waits for `route_stability_s`, resets only the affected uplink's
learned state, and gathers new samples. The learning pinger is kept awake even
when the normal idle policy would otherwise stop it. Only after sufficient
fresh evidence does the lifecycle return to `ACTIVE` or `STANDBY`.

These events invalidate stale learning:

- member offline and later online;
- L3 device change;
- source IPv4 change, including PPPoE reconnect;
- fwmark or routing-table change; and
- a different selected member.

Transport endpoint baselines, loaded samples, estimated quality, throughput
references, and adaptive-ceiling safe/failed bounds are reset together for
that instance. Another uplink continues independently.

## SQM ownership and calibration isolation

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
does not rewrite UCI. A candidate that changes route, address, or server fails
closed and writes no configuration.

## LuCI behavior

The setup view labels logical and physical topology, for example:

```text
wan -> pppoe-wan -> eth2
wanb -> eth0
```

The Multi-WAN creation mode detects unique enabled members and previews the
instance names, target devices, SQM sections, and conflicts before saving.
Status shows lifecycle state, route/member/device, source and external address,
fwmark, routing table, policy share, and the reason for standby/offline.
Graphs remain separate per instance and annotate route/lifecycle changes; all
history remains RAM-only.

## Diagnostics

Useful checks are:

```sh
/usr/libexec/cake-autorate-rs/mwan3-info
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
calibration is not yet a supported RC6 path. Load balancing can mark multiple
members active; each instance still remains bound to its configured member and
must own a distinct shaper.
