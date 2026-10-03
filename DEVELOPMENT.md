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

## Overflow checks

The daemon is built with integer overflow checks: an addition, subtraction or
multiplication that overflows panics instead of wrapping silently. The OpenWrt
SDK already turns this on for every Rust package
(`CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS=true` in
`feeds/packages/lang/rust/rust-values.mk`, identical in all twelve matrix SDKs),
so the packages have always carried the checks; `[profile.release]` in
`Cargo.toml` states it too, for builds outside the SDK. The environment variable
of the SDK takes precedence over the manifest.

A panic ends the thread that hit it. On the main thread of the controller it
exits the process and procd respawns it (`respawn 3600 5 5`: up to five restarts
in an hour); CAKE keeps the last rate it was given in the meantime, and the
panic message reaches the system log because procd captures stderr. A Speed
Test, Rating or Auto-Tune worker is a separate process: its job ends or goes to
recovery and the coordinator carries on. Silent wrapping could instead feed a
wrong number into a shaper rate or a deadline, which is why the checks stay on.

The cost was measured against a build of the same source with the SDK default
switched off in a private SDK copy:

- Size, twelve architectures: the Full binary grows by 0.6% to 1.1%, the Lite
  binary by 0% to 2.3% (the larger figures are one 64 KiB segment step; the
  packages, which are compressed, grow by 0.3% to 0.7%).
- CPU on the test VM (x86_64, 3 minutes idle and 4 minutes of repeated capped
  Speed Tests, each build measured twice): the controller used 0.39% (checks on)
  and 0.36% (off) of one core idle, 1.22% and 1.23% under load; the difference
  is inside the spread between repeated runs of the same build.
- Arithmetic-heavy code (the Auto-Tune search and the controller's text parsers,
  release profile, one pinned core): about 1.4% slower with the checks.

A release build must contain `-C overflow-checks=on` in the `rustc` command line
of the daemon in its SDK build log (`V=s`); check it for every architecture
rather than assuming the SDK default.

Every subtraction and multiplication that clippy's `arithmetic_side_effects`
lint flags was read against its guard: counter deltas use `checked_sub` or an
explicit reset check, offsets in the netlink and DNS parsers are compared with
the remaining length first, and search brackets are strict by construction.
None depends on an unvalidated input. The Auto-Tune search, the routed DNS
answer parser and the controller's text parsers were also run with the checks
on against about 3.4 million random and mutated inputs without a panic.

## MQTT over TLS tests

The MQTT publisher tests run a TLS broker inside the test process: a rustls
`ServerConfig` serving certificates that the tests generate with the `rcgen`
dev-dependency (a private CA, server certificates that are valid, expired, not
yet valid or issued for another name, and self-signed certificates with and
without the CA flag). No key is stored in the repository, and `rcgen` is not
compiled into the packages. The tests cover the trust rules (private CA, wrong
CA, built-in roots, unverified mode), the CA file rules, several MQTT packets
in one TLS record, the handshake probe of `--mqtt-status`, and the plan
encodings: a plain plan keeps the bytes of the release before TLS, checked
against a golden plan produced by that release, while a TLS plan uses the
version-2 encoding.

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
clock. None of them bounds an operation. The quality-grade windows do matter to
Rating: a step of the wall clock during a Rating capture makes the controller
drop the samples it has collected, so that run cannot complete and ends
incomplete or contaminated at its deadline instead of early; it can be
repeated. Speed Test and Full Auto-Tune measure on the boot clock; both were
run on the test VM with steps of up to five hours in either direction.

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
