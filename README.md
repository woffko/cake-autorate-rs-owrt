# cake-autorate-rs

OpenWrt package bundle for a Rust prototype of `cake-autorate` with a LuCI UI and UCI configuration.

The current target is OpenWrt 25.12.5 on `x86/64`. The daemon is intentionally kept small and currently uses only the Rust standard library plus OpenWrt userland tools.

## Repository Layout

This repository is organized as an OpenWrt package feed/SDK overlay. Each package directory follows the OpenWrt package documentation layout:

```text
package/<package-name>/Makefile
package/<package-name>/files/
package/<package-name>/src/
```

`files/` contains installed default config, init scripts, LuCI menu/ACL files, and LuCI views. `src/` contains bundled application source; OpenWrt explicitly supports bundled source code inside a package directory, commonly under `src/`.

## Contents

- `package/cake-autorate-rs` - Rust daemon package.
- `package/luci-app-cake-autorate-rs` - LuCI app for configuration and status.
- `/etc/config/cake-autorate` - UCI config installed by the daemon package.
- `/etc/init.d/cake-autorate` - procd service wrapper.
- `/usr/sbin/cake-autorated` - daemon binary.

## Current State

This is an MVP/prototype, not full feature parity with upstream bash `cake-autorate` yet.

Implemented:

- UCI-based config loading.
- Multiple enabled UCI sections via procd instances.
- `fping` RTT reflector probing, `fping-ts` and `tsping` ICMP timestamp OWD
  probing, explicit-server `irtt` OWD probing, plus a basic
  `pinger_method=ping` fallback.
- Active reflector health tracking and replacement for running `fping`,
  `fping-ts`, `tsping`, and `ping` probes: response-deadline offences,
  baseline/EWMA comparison, periodic replacement, optional reflector stats
  logging, and pinger restart with the next spare candidate.
- Runtime status JSON and LuCI status page expose active, spare, and bad
  reflector sets plus per-reflector samples, offence counters, and last RTT.
- sysfs RX/TX byte counter sampling.
- optional CPU usage sampling from `/proc/stat`, exposed in logs and status JSON.
- adaptive rate calculations using delay/load windows.
- `tc qdisc change ... cake bandwidth ...` shaper updates.
- daemon log rotation by age/size with best-effort gzip compression.
- JSON status file under `/var/run/cake-autorate/<instance>/status.json`.
- LuCI settings page with compact instance rows and modal tabs for detailed settings.
- LuCI cross-field validation for manual min/base/max rates, explicit
  download/upload interface conflicts, `ping` fallback pinger count, and
  duplicate managed SQM section ownership.
- LuCI setup wizard for creating instances, importing SQM rates, checking speed
  test backends, running a router-side speed test, and writing derived limits.
- LuCI Reflectors tab can check pinger backend availability and scan configured
  reflectors plus the upstream default anycast reflector pool, including RTT and
  ICMP timestamp capability, without adding hard dependencies. It shows RTT/OWD
  backend mode, install/manual-action hints, and can run `apk add fping` or
  `apk add irtt` for supported optional backends if they are missing.
- LuCI can apply the pinger planner recommendation into pending changes for an
  existing instance, and the create wizard writes pinger method, active pinger
  count, and reflector list for new instances.
- LuCI setup tab with the minimum recommended autorate fields from upstream:
  target interface, SQM download/upload, and min/base/max rates per direction.
- Router-side speed test helper with optional backend autodetection:
  `librespeed-cli`, `speedtest-go`, configured `iperf3`, then built-in HTTP
  fallback. Optional backend packages are not hard dependencies.
- Integrated SQM backend sync: each `cake-autorate` UCI section can own a matching
  `sqm` queue section.
- Optional MQTT publisher service: per-instance MQTT export reads daemon
  SUMMARY/CPU log records, publishes state via `mosquitto_pub`, and registers
  Home Assistant discovery sensors when enabled.
- Automatic interface preset: selecting the target interface fills
  `sqm_interface`, `ul_if`, and `dl_if=ifb4<target>`.
- Automatic SQM rate import from an existing `/etc/config/sqm` queue for the
  selected interface when available.
- LuCI status page with start, restart, stop actions.

Known limits:

- `pinger_method=ping` probes only the first selected reflector; use `fping`,
  `fping-ts`, `tsping`, or explicit-server `irtt` for concurrent probing.
- `pinger_method=irtt` requires the optional `irtt` package and at least one
  explicit `list irtt_server ...` entry. Generic DNS reflector pools are not
  used as IRTT servers.
- `fping-ts` and `tsping` depend on reflectors that answer ICMP timestamp
  probes; many public DNS anycast reflectors do not.
- `tsping` is runtime-detected and not a hard package dependency; install a
  compatible `tsping` binary manually where available before selecting it.
- reflector health/replacement is implemented as an MVP; `fping-ts` uses
  separate DL/UL OWD samples while RTT backends still use RTT/2 estimates.
- The LuCI planner can scan a broader upstream default candidate pool and apply
  the recommended pinger method, active count, and ordered reflector list.
- Use the external LibreQoS Internet Quality Test at https://test.libreqos.com/
  as a manual browser-side validation tool after configuring autorate. It is
  intentionally documented only, not integrated into the wizard or router-side
  speed test backend.
- Pinger auto-install is intentionally limited: the GUI can install/repair the
  supported `fping` package used by `fping`/`fping-ts` and the optional `irtt`
  package. `tsping` remains a manual binary install, and `irtt` is only ready
  when explicit IRTT servers are configured.
- advanced multi-WAN policy and log bundle export are not implemented.
- MQTT is an optional sidecar service rather than daemon core. It requires a
  configured broker, `log_to_file=1`, `output_summary_stats=1`, and
  `mosquitto_pub` from either `mosquitto-client-nossl` or
  `mosquitto-client-ssl`. CPU sensors additionally require
  `output_cpu_stats=1`.

SQM integration:

- `luci-app-cake-autorate-rs` is intended to be the single LuCI UI for SQM setup
  plus autorate control.
- Installing the LuCI package automatically installs `sqm-scripts`, which pulls
  the normal OpenWrt CAKE/IFB shaping stack.
- The LuCI package declares `PROVIDES:=luci-app-sqm` and `CONFLICTS:=luci-app-sqm`
  as the replacement intent. OpenWrt 25.12 APK package generation currently emits
  the provide metadata, but conflict metadata still needs verification in final
  packages.
- The UI includes the required `luci-app-sqm` settings: enable flag, interface,
  download/upload rates, debug logging, verbosity, qdisc, queue setup script,
  DSCP/ECN options, queue limits, latency targets, raw qdisc options, link layer
  mode, overhead, and advanced link layer parameters.
- `cake-autorate` UCI sections are the user-facing source of truth; the init
  script synchronizes matching `sqm` queue sections before starting SQM and
  autorate.
- Disabled `cake-autorate` sections do not mirror into SQM even when
  `manage_sqm=1`; stale owned SQM sections are cleaned up instead.
- Multiple interface/queue pairs are represented as multiple `cake_autorate`
  sections and shown in one compact LuCI grid.

## Runtime Dependencies

Required package dependencies:

- `uci`
- `fping`

LuCI package dependencies:

- `cake-autorate-rs`
- `luci-base`
- `sqm-scripts`

`sqm-scripts` pulls the required `tc`, CAKE, IFB, iptables, and related shaping
packages on OpenWrt.

Optional speed test backend packages:

- `librespeed-cli`
- `speedtest-go`
- `iperf3`
- `jsonfilter` is required to parse CLI backend JSON and is installed by the
  LuCI helper when installing an optional backend.

The LuCI Speed Test tab and setup wizard show backend availability and can run
`apk add` for the selected optional backend. The built-in HTTP backend requires
`curl`, `uclient-fetch`, or `wget`; it does not require an extra speed test
package.

Optional pinger backend binaries:

- `fping` with `--icmp-timestamp` support for `pinger_method=fping-ts`
- `tsping`
- `irtt` for `pinger_method=irtt`, with explicit `irtt_server` entries

The daemon accepts `pinger_method=tsping` when the binary is present in PATH and
`pinger_method=irtt` when `irtt` is installed and explicit IRTT servers are
configured. `tsping` remains optional because no supported OpenWrt package was
available on the current test router. The advanced LuCI Reflectors tab can
check pinger backend availability, show RTT/OWD and round-robin/individual
mode, install supported `fping`/`irtt` packages when needed, scan reflectors,
and apply the recommendation into pending changes. If `tsping` is manually
installed, the planner can use it as the timestamp probe path when
`fping --icmp-timestamp` is unavailable. The create wizard writes pinger
defaults and can run the same scan before creating a new instance.

Optional MQTT client packages:

- `mosquitto-client-nossl`
- `mosquitto-client-ssl`

The LuCI Logging tab shows MQTT availability and can install the default
`mosquitto-client-nossl` package. After setting `mqtt_enabled=1` and
`mqtt_host`, enable and start `/etc/init.d/cake-autorate-mqtt`; the publisher
creates Home Assistant discovery sensors and publishes instance state under the
configured base topic.

## Build In OpenWrt SDK

Use a clean OpenWrt 25.12.5 x86_64 SDK. The Rust feed builds a large host Rust/LLVM toolchain on first use, so cache the SDK or use a prepared build image for normal iteration.

Recommended feed workflow:

```sh
cd /path/to/openwrt-sdk
cp feeds.conf.default feeds.conf
cat /path/to/cake-autorate-rs/feeds.conf.example >> feeds.conf
./scripts/feeds update packages luci
./scripts/feeds update cake_autorate_rs
./scripts/feeds install rust fping luci-base
./scripts/feeds install cake-autorate-rs luci-app-cake-autorate-rs
make defconfig
make package/cake-autorate-rs/compile V=s -j1
make package/luci-app-cake-autorate-rs/compile V=s -j1
```

Overlay workflow during local development:

```sh
cp -a package/cake-autorate-rs /path/to/openwrt-sdk/package/
cp -a package/luci-app-cake-autorate-rs /path/to/openwrt-sdk/package/
```

Enable packages in `.config` when building as modules:

```text
CONFIG_PACKAGE_cake-autorate-rs=m
CONFIG_PACKAGE_luci-app-cake-autorate-rs=m
CONFIG_PACKAGE_fping=m
CONFIG_PACKAGE_rust=m
```

## Install

Copy the generated `.apk` files to the router and install them:

```sh
apk add --allow-untrusted /tmp/cake-autorate-rs-*.apk /tmp/luci-app-cake-autorate-rs-*.apk
```

Install backend dependencies if they are not already present:

```sh
apk add fping sqm-scripts
```

Optional speed test backends can be installed from LuCI or manually:

```sh
apk add librespeed-cli speedtest-go iperf3 jsonfilter
```

Optional MQTT support can be installed from LuCI or manually:

```sh
apk add mosquitto-client-nossl
```

Then edit `/etc/config/cake-autorate` or use LuCI:

```sh
uci set cake-autorate.primary.enabled='1'
uci commit cake-autorate
/etc/init.d/cake-autorate enable
/etc/init.d/cake-autorate restart
```

## Quick Checks

```sh
cake-autorated --instance primary --dump-config
cake-autorated --instance primary --once
cat /var/run/cake-autorate/primary/status.json
/usr/libexec/cake-autorate-rs/speedtest primary "" status auto
/usr/libexec/cake-autorate-rs/mqtt-status primary status
```

For a no-shaper smoke test, disable both shaper adjustment flags:

```sh
uci set cake-autorate.primary.adjust_dl_shaper_rate='0'
uci set cake-autorate.primary.adjust_ul_shaper_rate='0'
uci commit cake-autorate
cake-autorated --instance primary --once
```

For `ping` fallback and CPU/log smoke tests, use a temporary disabled-rate
instance with explicit counter paths, `adjust_dl_shaper_rate='0'`,
`adjust_ul_shaper_rate='0'`, `pinger_method='ping'`, `output_cpu_stats='1'`,
and a temporary `log_file_path_override`.

## Development Notes

Rust is a reasonable daemon language for this project because it provides one static-ish native binary, predictable memory safety, and better long-term maintainability than a large shell daemon. The main practical cost on OpenWrt is build complexity: the first SDK build of `rust/host` is heavy because it compiles Rust/LLVM tooling.

For faster iteration, keep a cached SDK or CI artifact with the Rust host toolchain already built.
