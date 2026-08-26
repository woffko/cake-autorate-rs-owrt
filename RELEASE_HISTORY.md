# Release history

The only supported downloadable build is
[`v1.0-rc27-r311-r120`](https://github.com/woffko/cake-autorate-rs-owrt/releases/tag/v1.0-rc27-r311-r120).
It contains daemon r311, Full LuCI r120, the manual-only Lite r311/r4 pair, the
12-ABI package matrix, checksums, and machine-readable release manifests.

Older release candidates are retained as source tags and engineering history,
not as supported downloads. Their separate GitHub Release entries and binary
assets were removed after the final Rust-migration release superseded them.
Do not install or mix packages from the historical tags below.

## Archived GitHub prereleases

| Tag | Published | Historical milestone |
|---|---|---|
| `v1.0-rc27-r22-r45` | 2026-07-30 | Added the evidence-driven Variable Link workflow and access-medium policy. |
| `v1.0-rc27-r235-r85` | 2026-08-13 | First broad native-migration and Full/Lite package matrix checkpoint. |
| `v1.0-rc27-r249-r97` | 2026-08-15 | Corrected native Rating, directional Review, and manual-only Lite release line. |
| `v1.0-rc27-r250-r98` | 2026-08-15 | Post-migration cleanup and Re-run correctness checkpoint. |
| `v1.0-rc27-r306-r120` | 2026-08-21 | Completed the Rust migration, Rating identity work, 12-ABI Full/Lite matrix and public-history sanitization. |

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
[`TESTING.md`](TESTING.md). Sections labelled with an older RC describe the
observed failure and acceptance evidence for that historical checkpoint; they
must not be read as current CLI, package, schema, or installation instructions.

## Retention policy

- Keep one current GitHub Release with verified downloadable assets.
- Keep historical git tags so source and test references remain reproducible.
- Keep old version numbers only in this history and the explicitly historical
  chronology in `TESTING.md`.
- Put all current installation, operation, dependency, and troubleshooting
  instructions in `README.md` and the feature guides without obsolete version
  branches.
