# Bounded Probe Ceiling

This document defines the runtime-only adaptive ceiling controller used on top
of the upstream-compatible CAKE autorate loop. The normal loop remains
responsible for reacting quickly to load and delay. The bounded controller only
decides how far that loop may search above the configured maximum.

This separation is deliberate: upstream
[`cake-autorate`](https://github.com/lynxthecat/cake-autorate) already supplies
the fast independent DL/UL load-and-delay feedback loop, while CAKE's
[`bandwidth` parameter](https://man7.org/linux/man-pages/man8/tc-cake.8.html)
is the enforced shaper rate. Bounded probing is a slow outer safety layer, not
a competing replacement controller.

## Safety invariants

- The configured maximum is the initial and reset ceiling.
- The effective ceiling never exceeds the absolute cap.
- Runtime learning never rewrites UCI.
- Only confirmed bufferbloat creates a failed upper bound. Isolated load or
  delay-classification noise is tolerated for a short grace window and must not
  erase the last proven-safe ceiling.
- A global probe-response gap aborts an active probe without treating the
  target as failed. A missed response from one reflector does not count as a
  global gap while other reflectors continue producing valid samples.
- A failed probe returns immediately to the last proven-safe ceiling.
- A stall resets all learned state to the configured maximum.
- A stale failed bound expires so a recovered variable-rate link can be probed
  again.

## Runtime state

Each direction tracks these values independently:

- `phase`: `cruise`, `qualify`, `probe_ramp`, `probe_observe`, or `backoff`.
- `safe_ceiling_kbps`: highest ceiling confirmed by a clean completed probe.
- `failed_ceiling_kbps`: lowest target that produced confirmed bufferbloat.
- `probe_target_kbps`: ceiling currently being tested.
- `effective_max_kbps`: ceiling currently enforced by the main controller.
- timestamps for qualification, phase entry, cooldown, and failed-bound age.
- the last transition reason for status and diagnostics.

## Transitions

1. `cruise -> qualify` when load is high, delay is acceptable, the shaper is
   within 2% of the safe ceiling, and another probe is possible.
2. `qualify -> probe_ramp` after continuous clean high load for the configured
   hold time.
3. The probe target is the smaller of:
   - `safe * (1 + probe_step_percent)`; and
   - the midpoint between safe and failed bounds, when a failed bound exists.
   Probing pauses when safe and failed are within 0.5% (or 1 Mbit/s) so the
   controller does not repeatedly touch an already-localized bottleneck.
4. `probe_ramp -> probe_observe` once the shaper reaches 98% of the target.
5. `probe_observe -> backoff` after the configured clean observation time. The
   target becomes the new safe ceiling.
6. Confirmed bufferbloat during a probe records the target as failed and enters
   backoff at the previous safe ceiling.
7. Loss of high load or acceptable delay starts a short grace window. Recovery
   within that window continues qualification/observation; a sustained loss
   aborts the probe to the safe ceiling without creating a failed bound.
8. `backoff -> cruise` after the configured cooldown.
9. A failed bound expires after its TTL. A stall or service restart resets all
   state to the configured maximum.

## Compatibility

The existing options remain valid:

- `adaptive_ceiling_enabled`
- `adaptive_ceiling_dl_cap_kbps`
- `adaptive_ceiling_ul_cap_kbps`
- `adaptive_ceiling_hold_time_s`
- `adaptive_ceiling_growth_percent`

`adaptive_ceiling_growth_percent` becomes the maximum open-ended probe step.
New optional values control observation duration, cooldown, and failed-bound
TTL. Missing new values receive conservative defaults. Existing UCI files are
not rewritten during migration.

## Acceptance gate

Deterministic simulations must cover a stable link, capacity increase,
capacity decrease, delay noise, probe loss, idle traffic, and a changing
asymmetric link. Compared with the legacy periodic-growth controller, bounded
probing must:

- retain a proven safe bound after transient noise;
- roll back a failed probe immediately;
- converge faster when spare capacity exists;
- bound repeated probe-induced bufferbloat once the bottleneck is localized;
- obey every safety invariant above.

Router rollout is allowed only after unit tests, simulations, package builds,
and a test-router integration gate pass.
