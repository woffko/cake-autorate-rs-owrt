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
- `fping` reflector probing.
- sysfs RX/TX byte counter sampling.
- adaptive rate calculations using delay/load windows.
- `tc qdisc change ... cake bandwidth ...` shaper updates.
- JSON status file under `/var/run/cake-autorate/<instance>/status.json`.
- LuCI settings page with the same main knobs exposed from the UCI config.
- LuCI status page with start, restart, stop actions.

Known limits:

- Only `pinger_method=fping` is implemented.
- `fping-ts`, `tsping`, `irtt`, and plain `ping` backends are not implemented.
- reflector replacement, health scoring, CPU stats, log export/compression, and MQTT integration are placeholders or not implemented.
- `tc` is treated as a runtime prerequisite when shaper adjustment is enabled, but is not a hard package dependency in this prototype to avoid pulling kernel scheduling packages during SDK-only builds.

## Runtime Dependencies

Required package dependencies:

- `uci`
- `fping`

Runtime tools needed for real shaping:

- `tc` from `tc-tiny`, `tc-full`, or an equivalent OpenWrt package.
- a configured CAKE qdisc on the configured download/upload interfaces.

LuCI package dependencies:

- `cake-autorate-rs`
- `luci-base`
- `rpcd`
- UCI RPC support on the target image.

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

Install `fping` and `tc` if they are not already present:

```sh
apk add fping tc-tiny
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
```

For a no-shaper smoke test, disable both shaper adjustment flags:

```sh
uci set cake-autorate.primary.adjust_dl_shaper_rate='0'
uci set cake-autorate.primary.adjust_ul_shaper_rate='0'
uci commit cake-autorate
cake-autorated --instance primary --once
```

## Development Notes

Rust is a reasonable daemon language for this project because it provides one static-ish native binary, predictable memory safety, and better long-term maintainability than a large shell daemon. The main practical cost on OpenWrt is build complexity: the first SDK build of `rust/host` is heavy because it compiles Rust/LLVM tooling.

For faster iteration, keep a cached SDK or CI artifact with the Rust host toolchain already built.
