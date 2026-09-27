# Release history

The current supported build is
[`v1.0-rc27-r320-r128`](https://github.com/woffko/cake-autorate-rs-owrt/releases/tag/v1.0-rc27-r320-r128).
It contains daemon r320, Full LuCI r128, the manual-only Lite r320/r8 pair,
the `speedtest-go` 1.7.10-r5 backend for every ABI, the 12-ABI package matrix,
checksums, and machine-readable release manifests.

This release makes Full Auto-Tune finish on real, imperfect networks: an
explicit total traffic choice, retries after a server failure, a switch to
another qualified server when one keeps failing, measured CAKE options after
an incomplete search, and tolerant handling of a raw control below the server
comparison, of mwan3 tracker flaps and of a route restore during a
measurement. It also adds explicit policy routes (for example a VPN) for the
controller, Speed Test, Auto-Tune and scheduled calibration.

The previous maintenance release
[`v1.0-rc27-r318-r127`](https://github.com/woffko/cake-autorate-rs-owrt/releases/tag/v1.0-rc27-r318-r127)
is superseded. Its tag remains; install a complete matching set from one
release and never mix revisions.

## Preserved rollback baseline

[`v1.0-rc27-r313-r120`](https://github.com/woffko/cake-autorate-rs-owrt/releases/tag/v1.0-rc27-r313-r120)
remains unchanged as the accepted Rust-migration baseline (daemon r313,
Full LuCI r120, Lite LuCI r4). Its tag and existing release assets are retained;
the new maintenance release does not overwrite them. Use a complete matching
pair when following a reviewed rollback procedure, never mix revisions.

Earlier candidates listed below are retained as source tags and engineering
history. Their separate GitHub Release entries and assets were removed after
the migration release superseded them; they are not supported downloads.

## Archived GitHub prereleases

| Tag | Published | Historical milestone |
|---|---|---|
| `v1.0-rc27-r22-r45` | 2026-07-30 | Added the evidence-driven Variable Link workflow and access-medium policy. |
| `v1.0-rc27-r235-r85` | 2026-08-13 | First broad native-migration and Full/Lite package matrix checkpoint. |
| `v1.0-rc27-r249-r97` | 2026-08-15 | Corrected native Rating, directional Review, and manual-only Lite release line. |
| `v1.0-rc27-r250-r98` | 2026-08-15 | Post-migration cleanup and Re-run correctness checkpoint. |
| `v1.0-rc27-r306-r120` | 2026-08-21 | Completed the Rust migration, Rating identity work, 12-ABI Full/Lite matrix and public-history sanitization. |
| `v1.0-rc27-r311-r120` | 2026-08-26 | Added exact Apply write-ahead recovery and post-lock readiness; superseded by r313 Full/Lite install sequencing. |

These entries are preserved by their git tags. The current release includes
all accepted functionality from them plus later identity, recovery,
portability, browser, package and documentation corrections.

## Earlier source milestones

The older `v1.0-rc6` through `v1.0-rc27` tags record development milestones
such as Multi-WAN isolation, native transport RTT, passive/guided Rating,
RAM-only graphs, Pareto Auto-Tune search, traffic profiles, directional SQM,
Variable Link, and the staged shell-to-Rust migration. They are not current
installation targets.

The detailed, chronological validation record remains in
[`TESTING_ARCHIVE.md`](TESTING_ARCHIVE.md). Sections labelled with an older RC describe the
observed failure and acceptance evidence for that historical checkpoint; they
must not be read as current CLI, package, schema, or installation instructions.

## Retention policy

- Keep one current supported GitHub Release with verified downloadable assets;
  retain the explicitly preserved r313 rollback baseline unchanged.
- Keep historical git tags so source and test references remain reproducible.
- Keep old version numbers only in this history and the explicitly historical
  chronology in `TESTING_ARCHIVE.md`.
- Put all current installation, operation, dependency, and troubleshooting
  instructions in `README.md` and the feature guides without obsolete version
  branches.

## RC27 r320/r128 release notes

This release is **RC27 r320/r128**: daemon package r320 and Full LuCI package
r128, with the parallel manual-only Lite pair r320/r8 and this project's
`speedtest-go` 1.7.10-r5 backend (CLI only) for every ABI. Upgrades keep all
configured instances.

Full Auto-Tune:

- Every initial run and rerun requires a **Total test traffic** choice for
  download plus upload: Unlimited, a preset or a custom total. A capped run is
  refused before traffic when it cannot hold the evidence minimum.
- The server comparison uses repeated full-rate runs on up to six servers and
  keeps every stable source within 80% of the best. **Retries after a server
  failure** (0–5, default 2) repeats a failed or timed-out run.
- When a direction still fails after its retries, it moves to the next
  qualified server and every measurement phase restarts on the new server
  combination; no comparison mixes two servers in one direction.
- When a search ends without a selection, Review keeps the exact measured CAKE
  rate pairs (lowest latency and highest throughput) next to SQM disabled;
  each lists what could not be measured.
- A raw control below 80% of the comparison no longer ends the run; its
  options carry an explicit acknowledgement. Gaming searches down to a
  capacity-aware floor and offers the fastest option too.
- A runtime restore during a measurement (for example after a route change)
  re-arms and repeats that measurement instead of failing the run.
- Comparable raw controls, loopback-safe traffic accounting and the backend's
  browser user agent fix earlier false stops.

Routing and UI:

- An mwan3 member whose tracker reports `disconnecting` still carries the
  route; short tracker flaps no longer pause autorate control.
- Explicit policy routes (source, table, mark and route DNS) for the Full
  controller, Speed Test, Auto-Tune and scheduled calibration.
- Standalone Speed Test asks for the same total traffic choice and reports its
  usage and stop reason.
- The wizard locks on start, shows plainer text and steps, links to the user
  guide, says when a stopped or refused test measured nothing, and can
  remember the traffic and retry choice per instance in the browser.

## RC27 r318/r127 release notes

That release was **RC27 r318/r127**: daemon package r318 and Full LuCI package
r127, with the parallel manual-only Lite pair r318/r7. It retains the complete
two-direction Rating authority, truthful staged Auto-Tune progress, a ranked
four-option Review including the measured mobile download-bypass topology, one
aggregate trade-off confirmation, and an Apply flow which verifies the runtime
and immediately reloads authoritative UCI without another button or tab
switch.

The native Apply transaction is write-ahead protected. Before mutating either UCI
package it durably records the exact original and candidate `cake-autorate` and
`sqm` bytes, modes, digests, request/job/worker identity, and selected option
manifest. Recovery classifies each live package as original, candidate, or
foreign, safely resolves every original/candidate mixed pair, and refuses an
unrecognized overwrite. Stale restore temporaries are cleaned only while the
same config-pair lock is held; unsafe links fail closed. A legacy recovery
record without candidate bytes remains rollback-only.

Apply, recovery, ordinary service start/reload, package replacement, and an
empty controller plan now share one state-driven readiness boundary. Success
is not published until the runtime lock is released and the exact expected
controller set is ready; a failed readiness proof becomes a failed terminal,
never a false Applied result. Package-upgrade deferral has its own typed receipt
so it cannot be confused with a genuine empty plan. No fixed retry timer or
second post-install confirmation is used. The focused crash-boundary suite,
live VM/router upgrades, browser audit, and Full/Lite 12-ABI verification are
recorded in
[Testing](TESTING.md). The README intentionally describes current behavior
instead of retaining a cumulative RC diary. Superseded milestones remain in
[Release history](RELEASE_HISTORY.md) and git tags, while
[GitHub Releases](https://github.com/woffko/cake-autorate-rs-owrt/releases)
contains the current release and the preserved r313 rollback baseline.

Fresh Full installation and Lite-to-Full replacement use the same readiness contract.
After OpenWrt's default package hook returns, the Full package now re-attests
the main controller, stops and settles any calibration instance that the
default hook already started, then enables and starts exactly one coordinator.
The in-place upgrade branch remains separate and performs no duplicate
readiness confirmation. Exact Full → Lite → manual stop/save/start → Full
testing now returns package status zero and restores the original UCI, services
and both CAKE qdiscs byte-for-byte.

This maintenance release adds client-disconnect isolation,
deadline-bounded process output, separate Status display preferences,
structured diagnostic redaction and truthful service-action errors. Dynamic
plain-text errors and diagnostics use text nodes instead of LuCI HTML strings.
Large exports and completed calibration results use authenticated streaming
with complete JSON validation, rather than rpcd's small command-output buffer.
Lost Apply replies are retried with the exact same request and handle. Native
SQM startup waits for the matching kernel topology event, and release of a
calibration owner clears stale operation state. Lite Save/Reset keeps UCI
transaction state consistent with the displayed form. These changes passed
the source, VM, physical-device and 12-ABI package gates described in
[Testing archive](TESTING_ARCHIVE.md#rc27-r318r127-release-acceptance).
