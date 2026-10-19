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

### Releases and the speedtest-go backend

The release set ships a `speedtest-go` APK for every ABI. Its content is
revision 6 with patches 100 to 140; patch 140 was added to revision 6 after the
first 12-ABI build of that revision, so the backend must be built again for all
twelve ABIs for the release (and its build information and ELF type checked),
not taken from an earlier build of revision 6.

Release acceptance on the test VM includes a Get rating on an emulated
dual-stack uplink: a global IPv6 address and an IPv6 default route on the WAN
through a gateway that never answers, with IPv4 to the nearby servers without an
AAAA record dropped, so that the backend chooses a dual-stack server itself. It
must complete with every refusal counter at zero. The same acceptance provokes
each refusal once (the backend before patch 140, a host route out through
another device, a `notrack` rule for the backend's user) and expects its code.

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

## Unsafe code

The daemon calls libc directly in a few places, under three rules:

- **Plain calls live in `src/sys.rs`.** A call with a simple signature (user,
  group and process ids, `flock`, `pidfd_open`, `poll`, `eventfd`, `timerfd`,
  `inotify`, `read` and `write` of counters and buffers, `kill`, `renameat2`,
  signal descriptors and a few more) is a safe function there, with one
  `unsafe` block and its own `// SAFETY:` comment. The wrappers keep the
  semantics of the call: they never retry (`EINTR` and `EAGAIN` reach the
  caller as `Interrupted` and `WouldBlock`), the OS error is read right after
  the call, and a new descriptor comes back as an `OwnedFd`. `kill` refuses a
  pid of zero or below and `kill_process_group` a group of 1 or below, since
  those would signal the caller's own group, init's group or every process.
  Each wrapper has a unit test.
- **Anything else keeps its `unsafe` block next to the code that owns the
  structure**, with a comment of its own: the netlink sockets,
  `getsockopt(SO_PEERCRED)`, `waitid`, `localtime_r` and `mktime`, the fcntl
  record lock, `read_unaligned` of inotify events, the two signal handlers and
  the `pre_exec` closures. A `pre_exec` closure runs between `fork` and `exec`
  in a process that may have been forked from a multi-threaded one, so it makes
  only async-signal-safe calls and returns errors without allocating (an OS
  error number, never `io::Error::new` with a message); its comment lists the
  calls. A signal handler does nothing but atomics and a `write(2)` on an
  eventfd.
- **Constants and types come from the `libc` crate**, not from local
  `extern "C"` declarations: several differ between targets (on MIPS
  `SOL_SOCKET` is `0xffff` and `SO_PEERCRED` is 18).

The source gate enforces this with `cargo clippy --all-targets -- -D
clippy::undocumented_unsafe_blocks` for the Full and the Lite build (the lint is
also `warn` in `Cargo.toml`, so an editor shows it), and with a check that no
local `extern "C"` block exists.

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

## Traffic accounting tables

Each operation counts its own traffic in temporary nftables tables:
`cake_st_<job>_<run>` for the speed-test backend (its user), `cake_pt_<job>_<run>`
for the probes of a job (root with a reserved group), and `cake_pm_<gid>` for
the permanent probes of a controller (`owned_route_rules.rs`). Each table has the
counters `rx` and `tx` and one counter per reason a packet of the owner is
refused (dropped):

| Counter | Chain | Refused packet | Code |
|---|---|---|---|
| `fault_family` | output | not IPv4 (except DNS to `::1` through `lo`) | `speedtest-accounting-non-ipv4-traffic` |
| `fault_egress` | egress (postrouting) | leaving through another device than the pinned one (except DNS to `127.0.0.1`/`::1`) | `speedtest-accounting-wrong-egress` |
| `fault_unregistered` | output | IPv4 that is not an accounted flow: `notrack`, conntrack zone other than 0, flow set full | `speedtest-accounting-flow-registration-failed` |

Permanent probes use `permanent-probe-non-ipv4-traffic`,
`permanent-probe-wrong-egress` and `permanent-probe-flow-accounting-failed`. The
counters carry the comment `<owner>:flow-v2`. Readers of the job tables also
accept the format before it (`<owner>:flow-v1`, a single `flow_fault` counter,
reported as `speedtest-accounting-flow-registration-failed`) and tables without
refusal counters, because a table of an older daemon can still exist after an
upgrade. A permanent owner reads only the table it installed itself, so it
accepts only the current format.

The unit test `t2_flow_accounting_export_exact_production_rules_for_kernel_fixture`
writes the exact batches (`main`, `mwan3`, both probe variants, the cutoff, and
an egress guard pinned to an unused device) into the directory named by
`CAKE_T2_NFT_BATCH_DIR`, so that they can be applied to a real nftables in a
throw-away user and network namespace, as described in the previous section,
and each refusal checked against its counter.

## Clocks and deadlines

An operation's limits must not depend on the wall clock: an NTP correction, a
manual `date -s`, or a router that boots with a stale time and synchronises
later can step it by seconds or by years. A step must neither expire a running
Speed Test, Rating or Auto-Tune early nor extend it.

- A request keeps `created_unix_ms` and `deadline_unix_ms`; the record format
  is unchanged. When the coordinator admits a job, `JournalStore::create`
  anchors the duration `deadline_unix_ms - created_unix_ms` on the kernel boot
  clock (`CLOCK_BOOTTIME` through `identity::monotonic_boot_ms`) and writes it as
  the file `deadline` beside the request. The file is private and written
  atomically: a header, the `boot_id` and `deadline_boot_ms`
  (`operations/deadline.rs`).
- The coordinator (`JournalStore::deadline`) and every worker
  (`deadline::load_for_worker`) turn that file into an `OperationDeadline` and
  compare it with `monotonic_boot_ms()`. The anchor belongs to one boot: another
  boot ID is refused. The journal scan already classifies such a job as
  `StaleBoot` and never resumes it (the journal lives under `/var/run`, which a
  reboot clears). A job of this boot that predates the anchor gets one from its
  remaining wall-clock time when the store opens (`anchor_jobs_without_deadline`);
  the lookup itself only reads, caches lasting failures and never writes. A job
  of another boot without an anchor is refused and gets none. The duration is
  counted from admission, so a wait before admission (for the coordinator
  socket, say) is not part of the deadline.
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

## Service start and the SQM hotplug

`/etc/init.d/cake-autorate start` runs `--service-lifecycle prepare-start`
(publish the configuration and the generation batch, start or adopt each
managed queue, classifier and MQTT plans), registers the controllers with
procd, and then runs `--service-lifecycle confirm-started`, which accepts the
batch once every controller is ready. The upstream iface hotplug
(`/etc/hotplug.d/iface/11-sqm`) restarts a queue with `run.sh stop` and then
`run.sh start`, outside our lock: during that gap the IFB device, the ingress
redirect or the state file (written last by `start-sqm`) are missing, and at
boot this happens when `wan` and `wan6` come up while `S97cake-autorate` runs.

- Both managed-SQM checks of `prepare-start` (after the SQM start and again
  after the sidecars) go through `settle_managed_sqm`: a failed observation
  waits for a real event (inotify on the state files and configuration, netlink
  on the interfaces) and then repeats the idempotent start action, which starts
  an absent queue and only attests an existing one. Each check is bounded by
  `SQM_START_ATTEST_TIMEOUT` (30 s); Busy and cancellation stay terminal, and
  there is no timer polling.
- `confirm-started` treats a controller that leaves readiness during the
  acceptance of the batch (`controller-batch-readiness-changed`) and a global
  lock held by a controller's own SQM check as progress to wait for, within
  `CONTROLLER_START_TIMEOUT` (90 s). Acceptance is resumable: every step
  rechecks the runtime first, so a later Ready repeats only what is left. A
  busy lock is retried after one second, because releasing it raises no event.
- A start that failed after publishing its batch, or whose controllers never
  became ready, leaves a pending batch without a predecessor
  (`Batch::unconfirmed_start`). The next `prepare-start` runs the ordinary Stop
  transaction under its own lock before it publishes a new batch; before this
  it refused with `service-start-pending-generation-recovery-required` until a
  stop or a reboot. rc.common holds `procd_lock` (flock on
  `/var/lock/procd_cake-autorate.lock`) for the whole run of every init action
  of the service, so no other start can be between its preparation and its procd
  registration at that point.
- `boot()` runs the first start as a child of the boot script. When it fails,
  a background loop asks `--service-lifecycle boot-retry <failed attempt>`,
  which owns the bound (three attempts in all) and the 15 s pause and logs
  `start failed at boot, retrying (n/3)`; each retry is an ordinary start in
  its own process. The retry subshell closes the inherited procd lock
  (descriptor 1000) before it creates its pipeline to `logger -t cake-autorate`:
  a copy kept by the loop or by `logger` would block every later init action of
  the service, the retried start and `status` included, until a reboot. The
  loop stops when the service is disabled. The shell keeps no `sleep`
  (`tests/shipped-shell-boundary.test.sh`); `tests/init-boot-retry.test.sh`
  runs the inherited-lock case under BusyBox ash.

### Reload (Save & Apply)

A reload pins every current managed queue when it begins (`RetainedSqmSet`:
the state file, the IFB and qdisc identities) and attests the pins up to its
confirmation, so that it never replaces or keeps a queue it did not prove. The
iface hotplug can replace a pinned queue at any of those points. The owner's
decision (L1) is to wait and pin again, never to accept a different queue:

- A failed pin or pin attestation goes through `settle_observation`: the first
  failure subscribes to the queue's events and looks again at once, every later
  one waits for a real event, bounded by `SQM_START_ATTEST_TIMEOUT` (30 s).
  Busy and cancellation stay terminal. The new pin is taken under the same
  lease, and only if the queue is again exactly the frozen recipe of the same
  generation (`retained SQM <instance> did not settle: ...` otherwise).
- Once the reload has published its files, the hotplug can restart a queue
  that the reload changes, from the new files. For such a queue the exact
  published recipe (`DesiredSqm`) is accepted instead of a pin.
- In `reconcile_sqm` the stop of an old queue waits out a hotplug gap and is
  skipped when the hotplug already runs exactly the desired queue
  (`stop_with_events`); the start of a new queue goes through
  `settle_managed_sqm_with`; the final readiness proof waits for an event
  (`await_ready_with`). The confirmation attests each reloaded queue with the
  same bounded wait.
- A start that meets a queue started from the files it replaced (by S50sqm or
  the hotplug before the publication) stops that queue through the replaced
  files' private frozen copies (`stop_managed_sqm_replaced_by`) and starts the
  published one.
- A reload that fails before it publishes its batch leaves the journal of its
  file transaction. Stop rolls it back once every consumer is stopped (`rolling
  back the files of a reload that did not complete`), and stops a queue the
  hotplug restarted from the published files through the current files'
  authority. After a reboot only the journal survives: Stop first stops the
  queues S50sqm started from those files, and the next start runs the Stop
  transaction before it publishes. No recovery record changed.
- Between fork and exec a helper of a controller (`tc`, `ip`, `uci`) still has
  the controller's command line; the controller scan ignores a process whose
  parent is a controller of the same instance, instead of reporting two
  controllers.
- The file transaction takes an exclusive `flock` on both packages to publish
  or roll back. libuci holds a shared one while it reads a package, which the
  SQM scripts do many times during a hotplug restart, and a release raises no
  event: the lock is retried every 10 ms, at most 2 s (`SOURCE_LOCK_WAIT`),
  before `uci-transaction-source-busy`. With one try, the publication and then
  its rollback found the source busy and the reload left
  `uci-transaction-recovery-required`.
- A reload that fails after it published its update leaves that update next
  to its accepted predecessor (both records under `/var/run`) and the journal
  of its files. The same reload resumes it. Anything else lets the ordinary
  Stop finish it (owner decision M1): `snapshot` accepts controllers of either
  generation, the plan covers the queues of both recipes
  (`OpenWrtServiceStop::stop_plan`), and `stop_sqm` tries the update's input,
  then the predecessor's, then the current files. `finish_stopped_publication`
  rolls the files back, and `Batch::retire_reload_update` abandons the update
  through the restore marker and retires the predecessor. A start runs this
  Stop first; a reload whose files changed since runs it and answers
  `service-reload-v1 legacy`, so the init script stops and starts the service
  from the current files. A selected Native Apply update keeps its own
  recovery.
- Stop, and the recoveries of a reload's preparation and of an unregistered
  publication, roll files back with `LaterCommits::Keep` (owner decision M2):
  a member the transaction did not change, or one committed again since, stays
  as it is; only the transaction's own untouched publications are restored, and
  the receipt names what is public afterwards. Before, a commit made after the
  failure refused the rollback for ever (`uci-transaction-recovery-conflict`,
  or `uci-transaction-recovery-pending` at every boot). The in-process rollback
  of an operation that has just failed stays strict.
- These rollbacks hold the upstream SQM run lock (`SqmRunLock`): run.sh
  creates `sqm-run.lock` in the SQM state directory before it reads
  `/etc/config/sqm` and removes it when it exits, retrying every second, ten
  times. A run that read the old files has finished when the lock is ours, so
  its queue fails the absence proof; a later run waits and reads the
  rolled-back files. Without it the iface hotplug at boot could read the
  failed reload's files before the rollback and start their queue after it,
  and no file described that queue any more. The lock is held only around
  observations and renames, carries a marker so that one left by a killed
  process of ours is taken over, and never removes run.sh's own.
- The readiness check reads each controller's `status.json`, which the
  controller replaces by rename. A file whose link count is 0 at the check was
  replaced after it was opened; the check waits for the next status instead
  of failing the start or the reload.
- The SQM event subscription reads the `ifindex` of each watched interface.
  sysfs refuses an attribute of a device being unregistered with `EINVAL`
  (`ENODEV` at open); such an interface counts as absent, like a missing one,
  instead of failing the wait with `invalid owned SQM interface index`.

## Notes

Rust is a reasonable daemon language for this project because it provides one static-ish native binary, predictable memory safety, and better long-term maintainability than a large shell daemon. The main practical cost on OpenWrt is build complexity: the first SDK build of `rust/host` is heavy because it compiles Rust/LLVM tooling.

For faster iteration, keep a cached SDK or CI artifact with the Rust host toolchain already built.

The speedtest-go backend overlay and its patches are described in
[package/speedtest-go/README.md](package/speedtest-go/README.md). Test
gates and acceptance evidence are summarised in [Testing](TESTING.md).
