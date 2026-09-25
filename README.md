# cake-autorate-rs

OpenWrt package bundle for a Rust implementation of `cake-autorate` with a
LuCI UI, UCI configuration, managed SQM lifecycle, and optional calibration.

## Project roots and acknowledgements

This project is a Rust/OpenWrt adaptation of
[`cake-autorate`](https://github.com/lynxthecat/cake-autorate), which was
created by [lynxthecat](https://github.com/lynxthecat) in 2021 and subsequently
developed by its community. The fast load-and-delay controller, terminology,
configuration model, and many defaults here deliberately follow that original
work. Development of this port started from the maintained
[`woffko/cake-autorate`](https://github.com/woffko/cake-autorate) fork.

Our sincere thanks go to lynxthecat for founding `cake-autorate`, to all of its
contributors for refining the algorithm in real networks, and to the CAKE,
OpenWrt, and `sqm-scripts` developers whose work provides the queueing and
shaping foundation. The port preserves the original fast load-and-delay
controller and builds an OpenWrt-native Rust runtime, managed SQM lifecycle,
LuCI/UCI workflow, structured Multi-WAN routing, Full Auto-Tune, transport
quality ratings, bounded ceiling discovery, RAM-only graphs, and optional
outbound DSCP profiles around it. The detailed boundary between inherited and
port-specific work is stated below; this project does not claim authorship of
the original cake-autorate concept.

## AI-assisted development disclosure

Substantial parts of this port were written and reviewed with OpenAI Codex and
Google Gemini working as paired development assistants. Anthropic Claude Opus
was also used for additional deep bug analysis and independent review. The
human project author defined the requirements and product logic, made the
design and safety decisions, controlled access to test equipment, reviewed the
results, and retained final authority over every accepted change. The
assistants provided implementation, analysis, testing, and independent review;
project ownership and responsibility remain with the human author.

## What it does

- Adjusts CAKE download and upload rates continuously from measured load and
  latency, following the original cake-autorate controller.
- Manages the SQM queue itself (CAKE, IFB, one or both directions) and restores
  it safely on start, stop, reload and package upgrade.
- Supports several uplinks: the main routing table, mwan3 members, or an
  existing explicit policy route (for example a VPN).
- **Full** package: connection rating, Speed Test, **Full Auto-Tune** with a
  Review step before anything is applied, scheduled calibration with traffic
  limits, RAM-only graphs and optional outbound DSCP traffic priorities.
- **Lite** package: the manual controller, routing, latency probes and SQM for
  small routers, without calibration features.

It targets OpenWrt 25.12 (apk). IPv6-only uplinks are not supported yet.

## Documentation

- [Quick setup guide](SETUP_GUIDE.md): install, first instance, first Auto-Tune.
- [User guide](USER_GUIDE.md): every LuCI screen, with stable section links.
- [Full Auto-Tune](AUTOTUNE.md): calibration job, proposals and Review.
- [Multi-WAN routing](MULTIWAN.md): `main`, `mwan3` and explicit policy routes.
- [Transport-aware quality](TRANSPORT_QUALITY.md): latency signals, ratings and
  scheduled Auto-Tune.
- [Traffic priorities](TRAFFIC_PRIORITIES.md): outbound DSCP rules.
- [Bounded probe ceiling](ADAPTIVE_CEILING.md) and
  [controller mathematics](ALGORITHM_MATH.md).
- [Features and limitations](FEATURES.md): the detailed implementation list.
- [Testing](TESTING.md) and [release history](RELEASE_HISTORY.md).
- [Development](DEVELOPMENT.md): repository layout and SDK builds.

## Supported platforms

Each release provides the daemon for these 12 OpenWrt 25.12 package ABIs. Run
`apk --print-arch` on the router and pick the file with the same suffix.

| APK suffix | Representative OpenWrt target |
|---|---|
| `x86_64` | `x86/64` |
| `aarch64_cortex-a53` | `bcm27xx/bcm2710` |
| `aarch64_cortex-a72` | `bcm27xx/bcm2711` |
| `aarch64_cortex-a76` | `bcm27xx/bcm2712` |
| `aarch64_generic` | `armsr/armv8`, `rockchip/armv8` |
| `arm_cortex-a7` | `mediatek/mt7629` |
| `arm_cortex-a7_neon-vfpv4` | `bcm27xx/bcm2709` |
| `arm_cortex-a9` | `bcm53xx/generic` |
| `arm_cortex-a9_vfpv3-d16` | `mvebu/cortexa9` |
| `arm_cortex-a15_neon-vfpv4` | `armsr/armv7` |
| `mips_24kc` | `ath79/generic` |
| `mipsel_24kc` | little-endian 24Kc targets |

## Packages

| Package | Full | Lite |
|---|---|---|
| Daemon | `cake-autorate-rs` (per ABI) | `cake-autorate-rs-lite` (per ABI) |
| LuCI | `luci-app-cake-autorate-rs` (noarch) | `luci-app-cake-autorate-rs-lite` (noarch) |
| Speed test backend | `speedtest-go` 1.7.10-r2 or newer (per ABI, from this project) | not used |

Full and Lite conflict; install one complete pair and never mix a Full daemon
with Lite LuCI. Other runtime dependencies (`sqm-scripts`, `fping`,
`nftables-json`, `uclient-fetch`, CAKE/IFB kernel modules, `tc`) come from the
router's OpenWrt package feeds, so the feeds must work.

The Full daemon requires this project's `speedtest-go` build (1.7.10-r2 or
newer). The OpenWrt feed only has 1.7.10-r1, which lacks the bounded-failure,
route-bound DNS and redirect fixes, so install the `speedtest-go` APK from the
same release together with the Full pair. Our build ships only the
`speedtest-go` CLI (about 8 MB installed).

## Install

Download the files for your ABI from the
[latest release](https://github.com/woffko/cake-autorate-rs-owrt/releases)
and copy them to the router. If the standalone SQM LuCI app is installed,
remove only that UI package; keep `sqm-scripts`:

```sh
apk info -e luci-app-sqm && apk del luci-app-sqm
```

Full (example for `aarch64_generic`; use the exact file names from the release):

```sh
cd /root
apk add --allow-untrusted --simulate \
  speedtest-go-*.apk cake-autorate-rs-*_aarch64_generic.apk luci-app-cake-autorate-rs-[0-9]*.apk
apk add --allow-untrusted \
  speedtest-go-*.apk cake-autorate-rs-*_aarch64_generic.apk luci-app-cake-autorate-rs-[0-9]*.apk
```

Lite:

```sh
apk add --allow-untrusted \
  cake-autorate-rs-lite-*_aarch64_generic.apk luci-app-cake-autorate-rs-lite-*.apk
```

`apk` extracts a whole package before replacing files, so a small root
filesystem needs free space for the new files while the old ones still exist.

Fresh installs have no instance and create no SQM queue. Create the first one
in **Network → CAKE Autorate SQM → Settings** as described in the
[quick setup guide](SETUP_GUIDE.md). Upgrades keep all configured instances.

### Switching between Full and Lite

The pairs conflict, so `apk` cannot swap them in one transaction. When the
current pair was installed from files, its dependencies are only implicit and
`apk del` would purge them too. Mark the shared ones as explicit first:

```sh
cp /etc/config/cake-autorate /root/cake-autorate.backup
apk add sqm-scripts fping
apk del --simulate luci-app-cake-autorate-rs cake-autorate-rs   # must list only these two
apk del luci-app-cake-autorate-rs cake-autorate-rs
apk add --allow-untrusted /root/cake-autorate-rs-lite-*.apk /root/luci-app-cake-autorate-rs-lite-*.apk
```

Use the mirrored names to return from Lite to Full (and include the
`speedtest-go` APK). Moving to Lite keeps manual instance settings but removes
the Rating/Auto-Tune services and their pages.

### Optional pinger backends

```sh
# fping-ts uses the installed fping binary; nothing extra is needed
apk add irtt  # also configure explicit IRTT servers and synchronized clocks
# tsping is a compatible binary installed manually; ping comes with the base system
```

## Quick checks

```sh
cake-autorated --instance wan_sqm --dump-config
cake-autorated --instance wan_sqm --once
cat /var/run/cake-autorate/wan_sqm/status.json
/usr/sbin/cake-autorated --calibrationctl summary
/usr/sbin/cake-autorated --calibrationctl speedtest-current wan_sqm
/usr/sbin/cake-autorated --calibrationctl rating-current wan_sqm
/usr/sbin/cake-autorated --mqtt-status wan_sqm status
/usr/sbin/cake-autorated --cpu-profile 30
```

`*-current` reports either the exact current operation or `state=idle`.
Job-specific `*-status`, `*-result`, and `*-cancel` commands require the public
job ID returned by the corresponding Start operation. These calibration
commands are intentionally unavailable in Lite.

For a no-shaper smoke test, disable both shaper adjustment flags:

```sh
uci set cake-autorate.wan_sqm.adjust_dl_shaper_rate='0'
uci set cake-autorate.wan_sqm.adjust_ul_shaper_rate='0'
uci commit cake-autorate
cake-autorated --instance wan_sqm --once
```

For `ping` fallback and CPU/log smoke tests, use a temporary disabled-rate
instance with explicit counter paths, `adjust_dl_shaper_rate='0'`,
`adjust_ul_shaper_rate='0'`, `pinger_method='ping'`, `output_cpu_stats='1'`,
and a temporary `log_file_path_override`.

## Relationship to upstream cake-autorate

The original [lynxthecat/cake-autorate](https://github.com/lynxthecat/cake-autorate)
solves the central variable-link problem: CAKE needs a bandwidth setting, while
LTE, 5G, Starlink, cable, and other links may change capacity faster than a
static setting can follow. It observes traffic load and reflector delay, then
adjusts download and upload rates independently between configured minimum,
baseline, and maximum values.

This port preserves that control model rather than replacing it with a generic
speed-test loop:

- per-direction minimum, baseline, and maximum CAKE rates;
- traffic-load detection from interface counters;
- ICMP RTT or timestamp-based one-way-delay evidence from multiple reflectors;
- fast rate increases under clean load, immediate reduction on confirmed
  bufferbloat, low-load return toward baseline, and a refractory interval;
- idle/stall handling, reflector health/replacement, stale-sample rejection,
  and wire-size/CAKE-overhead compensation;
- a hard configured maximum by default. The Rust-only adaptive ceiling remains
  a separate, explicit opt-in.

The port then adds an OpenWrt-native management and measurement layer around
that controller:

| Area | Original project | Added by this Rust/OpenWrt port |
|---|---|---|
| Runtime | Concurrent Bash processes and external pingers | One memory-safe Rust controller per UCI/procd instance, bounded parsers and native route-bound transport probes |
| Platform | OpenWrt and Asuswrt-Merlin | OpenWrt 25.12 package feed/SDK integration for the published x86_64, AArch64, ARMv7 and MIPS ABI matrix; Asuswrt-Merlin is not supported |
| Configuration | Shell configuration files | UCI source of truth, procd lifecycle, rpcd ACLs, and an integrated LuCI interface |
| SQM ownership | Works with an existing CAKE/SQM setup | Creates, synchronizes, verifies, repairs, and uniquely owns each managed SQM/CAKE/IFB/redirect path while leaving unrelated queues alone |
| Multiple links | Multiple script instances are possible | Structured main-table or nftables mwan3 member routing, one isolated instance/state/queue per uplink, route identity checks, failover states, and cross-WAN ownership guards |
| Initial tuning | User chooses min/base/max from observed link behavior | Manual wizard, backend-aware speed test, and Full Auto-Tune with separate Gaming, Best overall, Variable link, and Fair throughput/latency objectives |
| Auto-Tune safety | Not an upstream feature | RAM-only jobs, background-traffic accounting, ICMP plus native transport evidence, bounded per-direction frontier search, typed validation, exact proposal review, crash recovery, and guarded UCI apply |
| Quality | Delay drives the controller | LibreQoS-style complete DL/UL detected grades, passive client-traffic episodes, guided **Get rating**, CURRENT/LAST KNOWN semantics, and optional transport-aware ceiling control |
| Maximum discovery | Configured maximum is fixed | Optional bounded adaptive ceiling starts from exact shaped evidence and learns only below measured-raw and optional service caps, without rewriting UCI |
| Observability | Detailed logs and external analysis tools | Live JSON status, component-level Services health, CPU/softirq and CAKE diagnostics, redacted export, and opt-in RAM-only synchronized latency/CPU/traffic graphs |
| Traffic policy | Relies on the surrounding CAKE/SQM configuration | Optional outbound-only nftables DSCP profiles for Gaming, Best overall, Fair, and editable Custom rules, with runtime checksum attestation and no second qdisc owner |
| Automation | Primarily controller runtime | Scheduled quiet-window Auto-Tune, per-instance speed-test server caching, package/backend checks, and safe review-only versus validated auto-apply modes |
| Integrations | Upstream logging/analysis ecosystem | Optional MQTT/Home Assistant publisher and a LuCI replacement surface for the managed SQM settings |

The additions are intentionally bounded. This is not a drop-in rewrite of every
upstream script, log-analysis utility, or platform integration. In particular,
the upstream Asuswrt-Merlin path is absent, upstream configuration files are not
accepted verbatim, and adaptive ceiling, Full Auto-Tune, transport grades,
Multi-WAN routing, graphs, and the native DSCP classifier are port-specific.
The controller mathematics and inherited terminology are documented in
[Controller mathematics](ALGORITHM_MATH.md); port-specific safety boundaries
are documented in the linked feature references above.
