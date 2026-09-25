# Development

## Repository layout

This repository is organized as an OpenWrt package feed/SDK overlay. Each package directory follows the OpenWrt package documentation layout:

```text
package/<package-name>/Makefile
package/<package-name>/files/
package/<package-name>/src/
```

`files/` contains installed default config, init scripts, LuCI menu/ACL files, and LuCI views. `src/` contains bundled application source; OpenWrt explicitly supports bundled source code inside a package directory, commonly under `src/`.

## Contents

- `package/cake-autorate-rs` - Rust daemon package.
- `package/luci-app-cake-autorate-rs` - Full LuCI app.
- `package/luci-app-cake-autorate-rs-lite` - minimal manual-only LuCI app.
- `/etc/config/cake-autorate` - UCI config installed by the daemon package.
- `/etc/init.d/cake-autorate` - procd service wrapper.
- `/usr/sbin/cake-autorated` - daemon binary.

## Build in the OpenWrt SDK

Use a clean OpenWrt 25.12 SDK whose package architecture matches the required
APK suffix. The Rust feed builds a large host Rust/LLVM toolchain on first use,
so cache the SDK or use a prepared build image for normal iteration.

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

For the manual-only variant, select/build `cake-autorate-rs-lite` and
`luci-app-cake-autorate-rs-lite` instead. The daemon is compiled with Rust
default features disabled, so this is a real smaller binary rather than only a
hidden menu:

```sh
./scripts/feeds install cake-autorate-rs-lite luci-app-cake-autorate-rs-lite
make package/cake-autorate-rs/compile V=s -j1
make package/luci-app-cake-autorate-rs-lite/compile V=s -j1
```

Overlay workflow during local development:

```sh
cp -a package/cake-autorate-rs /path/to/openwrt-sdk/package/
cp -a package/luci-app-cake-autorate-rs /path/to/openwrt-sdk/package/
cp -a package/luci-app-cake-autorate-rs-lite /path/to/openwrt-sdk/package/
```

Enable packages in `.config` when building as modules:

```text
CONFIG_PACKAGE_cake-autorate-rs=m
CONFIG_PACKAGE_luci-app-cake-autorate-rs=m
CONFIG_PACKAGE_fping=m
CONFIG_PACKAGE_rust=m
```

For Lite select `CONFIG_PACKAGE_cake-autorate-rs-lite=m` and
`CONFIG_PACKAGE_luci-app-cake-autorate-rs-lite=m` instead of the two Full
package symbols.

## Notes

Rust is a reasonable daemon language for this project because it provides one static-ish native binary, predictable memory safety, and better long-term maintainability than a large shell daemon. The main practical cost on OpenWrt is build complexity: the first SDK build of `rust/host` is heavy because it compiles Rust/LLVM tooling.

For faster iteration, keep a cached SDK or CI artifact with the Rust host toolchain already built.

The speedtest-go backend overlay and its patches are described in
[package/speedtest-go/README.md](package/speedtest-go/README.md). Test
gates and acceptance evidence are summarised in [Testing](TESTING.md).
