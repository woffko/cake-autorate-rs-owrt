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

## Tests against a real nftables

Two ignored unit tests apply the traffic-classifier rules to a real nftables in
a throw-away network namespace. They never touch the host firewall: they refuse
to run in the namespace named by `CAKE_TEST_PARENT_NETNS`.

Get an `nft` binary without installing it, for example with
`apt-get download nftables libnftables1 libnftnl11` and `dpkg-deb --extract`
into a temporary directory, plus a wrapper script that sets `LD_LIBRARY_PATH`.
Then run the test binary in new user and network namespaces:

```sh
unshare --user --map-user=0 --map-group=0 --net env \
  CAKE_TEST_PARENT_NETNS="$(readlink /proc/self/ns/net)" \
  CAKE_TEST_NATIVE_NFT=/absolute/path/to/nft \
  package/cake-autorate-rs/src/target/debug/deps/cake_autorated-<hash> \
  --ignored operations::traffic_classifier
```

## Clocks and deadlines

An operation's limits must not depend on the wall clock: an NTP correction, a
manual `date -s`, or a router that boots with a stale time and synchronises
later can step it by seconds or by years. A step must neither expire a running
Speed Test, Rating or Auto-Tune early nor extend it.

- A request keeps `created_unix_ms` and `deadline_unix_ms`; the record format
  is unchanged. When the coordinator admits a job, `JournalStore::create`
  anchors the duration `deadline_unix_ms - created_unix_ms` on the kernel boot
  clock (`/proc/uptime` through `identity::monotonic_boot_ms`) and writes it as
  the file `deadline` beside the request. The file is private and written
  atomically: a header, the `boot_id` and `deadline_boot_ms`
  (`operations/deadline.rs`).
- The coordinator (`JournalStore::deadline`) and every worker
  (`deadline::load_for_worker`) turn that file into an `OperationDeadline` and
  compare it with `monotonic_boot_ms()`. The anchor belongs to one boot: another
  boot ID is refused. The journal scan already classifies such a job as
  `StaleBoot` and never resumes it (the journal lives under `/var/run`, which a
  reboot clears). A job that predates the anchor gets one from its remaining
  wall-clock time at the first lookup.
- A format that carries a wall-clock deadline and cannot change, the rating
  capture request `job|mode|deadline_s|phase|...`, is written from the remaining
  boot time at every publication. The controller reads the value when the
  request appears or changes and follows it on its monotonic clock
  (`RatingCaptureWindow`).
- The instance's runtime snapshot is stamped with the wall clock
  (`updated_unix_ms`) and its format is unchanged. `rating::publication_age`
  classifies a stamp as fresh, stale or ahead; every reader treats stale and
  ahead as "not current yet" and waits for the next publication.
- Scheduling is calendar work (windows, due slots, daily and monthly budgets)
  and stays on the wall clock. The scheduler pass refuses a wall clock before
  2026-01-01 (`MIN_PLAUSIBLE_WALL_CLOCK_UNIX_S`) and a due slot is blocked
  while the clock is behind the last success. A plausible but wrong time cannot
  be told from a right one.

| Place | What is compared | Clock now |
| --- | --- | --- |
| `JournalStore::create` | request duration | anchored on the boot clock at admission |
| coordinator `runtime_preflight_ready` | queued job against its deadline | boot clock |
| coordinator `next_event_timeout` | time to the nearest job deadline | boot clock |
| coordinator permit builders (`build_runtime_override_permit`, `assemble_bootstrap_runtime_override_permit`) | remaining time for the runtime permit | boot clock |
| `native_runtime_snapshot_readiness_at` | deadline; age of the instance snapshot | boot clock; wall clock for the stamp, ahead is a wait |
| `rating::wait_for_permit` (Rating, Speed Test and Auto-Tune workers) | wait for the worker permit | boot clock |
| `run_rating`, `wait_for_quiet` (sub-deadline), `wait_capture_phase`, `wait_for_rating_snapshot` | loop limits | boot clock |
| `rating::CaptureGuard::publish` | capture request deadline | derived from boot time left at each publication |
| controller `rating_capture_admissible` | capture request deadline | wall clock read once, then monotonic |
| `speedtest` session, route wait, backend run limit | route wait, backend deadline | boot clock |
| `full_autotune::wait_for_runtime_permit` | wait for the runtime permit | boot clock |
| `full_autotune::runtime_measurement_basis_ready` | age of the instance snapshot | wall clock stamp, ahead is a wait |
| bootstrap runtime owner | owner wake-up, permit publication limit | boot clock |
| scheduler pass | due slots, windows, budgets | wall clock, with the floor above |

Controller sample timestamps (reflector response age, quality-grade windows),
graph history, status timestamps, the configuration-transfer candidate expiry,
the log-bundle header and the qdisc capability cache keep reading the wall
clock; none of them bounds an operation.

Unit tests script both clocks with `identity::test_clock::ScriptedClocks`
(per thread, restored on drop): `advance` moves both, `step_wall_forward` and
`step_wall_back` move the wall clock only. They check that deadlines, waits and
the capture request do not change under a step, and that snapshot stamps ahead
of or behind the reader's clock are waits, not failures.

## Notes

Rust is a reasonable daemon language for this project because it provides one static-ish native binary, predictable memory safety, and better long-term maintainability than a large shell daemon. The main practical cost on OpenWrt is build complexity: the first SDK build of `rust/host` is heavy because it compiles Rust/LLVM tooling.

For faster iteration, keep a cached SDK or CI artifact with the Rust host toolchain already built.

The speedtest-go backend overlay and its patches are described in
[package/speedtest-go/README.md](package/speedtest-go/README.md). Test
gates and acceptance evidence are summarised in [Testing](TESTING.md).
