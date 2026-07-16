#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
helper="$base/root/usr/libexec/cake-autorate-rs/apply-guard"
fixtures="$base/tests/fixtures/apply-guard"
runtime_lock="$(CDPATH= cd -- "$base/../cake-autorate-rs/files/usr/libexec/cake-autorate-rs" && pwd)/runtime-lock"
work="${TMPDIR:-/tmp}/cake-apply-guard-test.$$"
config="$work/config"
autotune="$work/autotune"
guard="$work/guard"
mkdir -p "$config" "$autotune/wan_sqm" "$work/sys/pppoe-wan" "$work/sys/ifb4pppoe-wan" "$work/proc/200"
trap 'rm -rf "$work"' EXIT INT TERM

export PATH="$fixtures:$PATH"
export CAKE_AUTORATE_CONFIG_DIR="$config"
export CAKE_AUTORATE_AUTOTUNE_DIR="$autotune"
export CAKE_AUTORATE_APPLY_GUARD_DIR="$guard"
export CAKE_AUTORATE_AUTOTUNE="$fixtures/autotune"
export CAKE_AUTORATE_SPEEDTEST="$fixtures/speedtest"
export CAKE_AUTORATE_JSONFILTER="$fixtures/jsonfilter"
export CAKE_AUTORATE_RUNTIME_LOCK_LIB="$runtime_lock"
export CAKE_AUTORATE_DAEMON=/usr/sbin/cake-autorated
export CAKE_AUTORATE_TC="$fixtures/tc"
export CAKE_AUTORATE_SYS_CLASS_NET="$work/sys"
export CAKE_AUTORATE_PROC_ROOT="$work/proc"
export CAKE_AUTORATE_SQM_INIT="$fixtures/sqm-init"
export APPLY_GUARD_SQM_INIT_STATE="$work/sqm-init.state"
export CAKE_AUTORATE_APPLY_GUARD_TTL_S=180
export CAKE_AUTORATE_APPLY_GUARD_POSTCHECK_WAIT_S=1
export CAKE_AUTORATE_APPLY_GUARD_ORPHAN_AGE_S=60
export APPLY_GUARD_DAEMON_RUNNING=0
printf '%s\n' disabled > "$APPLY_GUARD_SQM_INIT_STATE"
printf '%s\n' '200 (cake-autorated) S 1 1 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 424242' > "$work/proc/200/stat"

cat > "$config/cake-autorate" <<'EOF'
wan_sqm|__type|cake_autorate
wan_sqm|enabled|1
wan_sqm|wan_if|pppoe-wan
wan_sqm|route_mode|main
wan_sqm|sqm_section|cake_wan_sqm
wan_sqm|sqm_qdisc|cake
wan_sqm|sqm_script|piece_of_cake.qos
wan_sqm|sqm_linklayer|none
wan_sqm|sqm_overhead|0
wan_sqm|sqm_tcMPU|0
wan_sqm|adaptive_ceiling_enabled|0
wan_sqm|unrelated_preserved|keep-me
EOF
cat > "$config/sqm" <<'EOF'
cake_wan_sqm|__type|queue
cake_wan_sqm|_cake_autorate_managed|wan_sqm
cake_wan_sqm|enabled|1
cake_wan_sqm|interface|pppoe-wan
cake_wan_sqm|download|50000
cake_wan_sqm|upload|10000
EOF

fingerprint="sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
cat > "$autotune/wan_sqm/result.json" <<EOF
{
  "state":"complete", "schema_version":3, "auto_apply_eligible":true,
  "phase_evidence_complete":true, "phase_contamination_seen":false,
  "runtime_restored":true, "recovery_pending":false,
  "configuration_written":false, "conservative":false,
  "confidence_mode":"normal", "job_id":"wan_sqm",
  "target_interface":"pppoe-wan", "resolved_interface":"pppoe-wan",
  "route_interface":"pppoe-wan", "route_mode":"main", "mwan3_member":"",
  "source_ip":"192.0.2.10", "external_ip":"192.0.2.20",
  "route_identity":"main||pppoe-wan|192.0.2.10||main",
  "config_fingerprint":"$fingerprint",
  "runs":[{"backend":"speedtest-go","server_id":"17372"}],
  "validation":{"pass":true,"contaminated":false,"correction":{"action":"none","feasible":true}},
  "pinger_plan":{"recommended_method":"fping","recommended_no_pingers":3,
    "recommended_reflectors":["1.1.1.1","9.9.9.9","8.8.8.8"]},
  "proposal":{
    "download":{"minimum_kbps":40000,"base_kbps":80000,"maximum_kbps":90000,
      "absolute_cap_kbps":95000,"observed_low_kbps":85000,"observed_median_kbps":88000},
    "upload":{"minimum_kbps":10000,"base_kbps":20000,"maximum_kbps":24000,
      "absolute_cap_kbps":25000,"observed_low_kbps":21000,"observed_median_kbps":23000},
    "active_threshold_kbps":2000,
    "thresholds_ms":{"adjust_up":6,"delay":15,"adjust_down":40},
    "adaptive_ceiling":{"enabled":true,"hold_s":15,"growth_percent":3,
      "probe_s":8,"cooldown_s":45,"failed_bound_ttl_s":900},
    "link":{"kind":"ethernet","layer":"none","overhead":0,"mpu":0}
  }
}
EOF
chmod 600 "$autotune/wan_sqm/result.json"

cp "$autotune/wan_sqm/result.json" "$work/result.valid"
sed -i 's/"base_kbps":80000/"base_kbps":0/' "$autotune/wan_sqm/result.json"
if $helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 "$fingerprint" >/dev/null 2>&1; then
	echo "apply guard armed an invalid proposal" >&2
	exit 1
fi
if find "$guard" -mindepth 1 -print -quit 2>/dev/null | grep -q .; then
	echo "failed arm leaked a root-owned apply token" >&2
	exit 1
fi
cp "$work/result.valid" "$autotune/wan_sqm/result.json"

# The deterministic enrollment name is reserved. Never reuse or later delete
# a preexisting user section with the same name, regardless of its type.
cp "$config/sqm" "$work/sqm.before-collision"
uci -q set sqm.cake_autorate_apply_wan_sqm=queue
uci -q set sqm.cake_autorate_apply_wan_sqm.enabled=1
uci -q set sqm.cake_autorate_apply_wan_sqm.interface=user-owned
cp "$config/sqm" "$work/sqm.with-collision"
if $helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 "$fingerprint" >/dev/null 2>&1; then
	echo "apply guard reused a preexisting SQM enrollment section" >&2
	exit 1
fi
cmp -s "$work/sqm.with-collision" "$config/sqm" || {
	echo "rejected SQM enrollment collision changed user configuration bytes" >&2
	exit 1
}
cp "$work/sqm.before-collision" "$config/sqm"

arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 "$fingerprint")"
printf '%s\n' "$arm" | grep -q '"state":"armed"'
token="$(printf '%s\n' "$arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
[ "${#token}" -eq 64 ]

uci -q set sqm.cake_autorate_apply_wan_sqm=cake_autorate_apply_guard
uci -q set sqm.cake_autorate_apply_wan_sqm._autotune_apply_guard=1
uci -q set sqm.cake_autorate_apply_wan_sqm._autotune_apply_job=wan_sqm
uci -q set sqm.cake_autorate_apply_wan_sqm._autotune_apply_fingerprint="$fingerprint"
uci -q set sqm.cake_autorate_apply_wan_sqm._autotune_apply_token="$token"

# Simulate rpcd applying exactly the staged CAKE section. SQM remains at its
# arm-time value until the guarded init hook is allowed to run.
cp "$guard/$token/expected/cake-autorate" "$config/cake-autorate"
verified="$($helper verify-init)" || { printf '%s\n' "$verified" >&2; exit 1; }
printf '%s\n' "$verified" | grep -q '"state":"verified"'

# Any stale or additional protected-section edit is rejected before init.
uci -q set cake-autorate.wan_sqm.max_dl_shaper_rate_kbps=89999
if $helper verify-init >/dev/null 2>&1; then
	echo "apply guard accepted a stale CAKE manifest" >&2
	exit 1
fi
cp "$guard/$token/expected/cake-autorate" "$config/cake-autorate"

# Ordered lists are semantic: active/spare reflector order must not be hidden
# by canonical option sorting.
uci -q delete cake-autorate.wan_sqm.reflector
uci -q add_list cake-autorate.wan_sqm.reflector=8.8.8.8
uci -q add_list cake-autorate.wan_sqm.reflector=9.9.9.9
uci -q add_list cake-autorate.wan_sqm.reflector=1.1.1.1
if $helper verify-init >/dev/null 2>&1; then
	echo "apply guard accepted a reordered reflector list" >&2
	exit 1
fi
cp "$guard/$token/expected/cake-autorate" "$config/cake-autorate"

uci -q set cake-autorate.unrelated=cake_autorate
uci -q set cake-autorate.unrelated.enabled=0
if $helper verify-init >/dev/null 2>&1; then
	echo "apply guard accepted an unrelated CAKE package edit" >&2
	exit 1
fi
cp "$guard/$token/expected/cake-autorate" "$config/cake-autorate"
uci -q set sqm.unrelated=queue
uci -q set sqm.unrelated.enabled=0
if $helper verify-init >/dev/null 2>&1; then
	echo "apply guard accepted an unrelated SQM package edit" >&2
	exit 1
fi
cp "$guard/$token/expected-pre/sqm" "$config/sqm"

# Route/source/external identity remains part of every pre/post attestation.
if APPLY_GUARD_EXTERNAL_IP=198.51.100.44 $helper verify-init >/dev/null 2>&1; then
	echo "apply guard accepted a changed external address" >&2
	exit 1
fi
$helper verify-init >/dev/null

# Tokens are time-bounded even when every marker field still matches.
expires="$(sed -n 's/^expires_epoch=//p' "$guard/$token/meta")"
sed -i 's/^expires_epoch=.*/expires_epoch=0/' "$guard/$token/meta"
if $helper verify-init >/dev/null 2>&1; then
	echo "expired apply token was accepted" >&2
	exit 1
fi
sed -i "s/^expires_epoch=.*/expires_epoch=$expires/" "$guard/$token/meta"

# Simulate init materializing the expected managed queue, then require the
# exact queue plus daemon state before the UCI transaction may be confirmed.
cp "$guard/$token/expected/sqm" "$config/sqm"
$helper verify-init >/dev/null
export APPLY_GUARD_DAEMON_RUNNING=1
other_token=ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff
cp -a "$guard/$token" "$guard/$other_token"
if $helper postcheck "$other_token" >/dev/null 2>&1; then
	echo "postcheck accepted a token different from the live marker" >&2
	exit 1
fi
$helper abort "$other_token" >/dev/null

# A live marker owns its root token. Neither abort nor expired-token GC may
# invalidate it and leave persistent UCI state tokenless.
if $helper abort "$token" >/dev/null 2>&1; then
	echo "abort removed a token referenced by live apply markers" >&2
	exit 1
fi
[ -d "$guard/$token" ]
postcheck="$($helper postcheck "$token")" || { printf '%s\n' "$postcheck" >&2; exit 1; }
printf '%s\n' "$postcheck" | grep -q '"state":"verified"'

# Runtime proof requires exactly one fresh daemon and exactly one root CAKE
# qdisc per managed direction.
mkdir -p "$work/proc/201"
printf '%s\n' '201 (cake-autorated) S 1 1 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 525252' > "$work/proc/201/stat"
sed -i 's/^original_daemon_identity=.*/original_daemon_identity=200:424242,/' "$guard/$token/meta"
if APPLY_GUARD_DAEMON_PID="200
201" $helper postcheck "$token" >/dev/null 2>&1; then
	echo "postcheck accepted a stale and fresh daemon simultaneously" >&2
	exit 1
fi
sed -i 's/^original_daemon_identity=.*/original_daemon_identity=none/' "$guard/$token/meta"
if APPLY_GUARD_TC_MODE=child $helper postcheck "$token" >/dev/null 2>&1; then
	echo "postcheck accepted child-only CAKE qdiscs" >&2
	exit 1
fi
if APPLY_GUARD_TC_MODE=duplicate $helper postcheck "$token" >/dev/null 2>&1; then
	echo "postcheck accepted duplicate root CAKE qdiscs" >&2
	exit 1
fi
uci -q set sqm.cake_wan_sqm.upload=19999
if $helper postcheck "$token" >/dev/null 2>&1; then
	echo "apply guard accepted a mismatched managed SQM queue" >&2
	exit 1
fi
cp "$guard/$token/expected/sqm" "$config/sqm"
$helper postcheck "$token" >/dev/null

# RPC rollback must restore both complete packages, including removing the
# SQM enrollment marker. Keep the token until that exact state is proven.
cp "$guard/$token/original/cake-autorate" "$config/cake-autorate"
cp "$guard/$token/original/sqm" "$config/sqm"
export APPLY_GUARD_DAEMON_RUNNING=0
$helper verify-rollback "$token" | grep -q '"state":"rolled-back"'
cp "$guard/$token/expected/cake-autorate" "$config/cake-autorate"
cp "$guard/$token/expected/sqm" "$config/sqm"
export APPLY_GUARD_DAEMON_RUNNING=1
$helper postcheck "$token" >/dev/null

# Once postcheck is complete, prepare-confirm removes both persistent markers
# while rpcd rollback is still armed and records the exact marker-free state.
prepare="$(APPLY_GUARD_NOW_EPOCH=$((expires + 1)) $helper prepare-confirm "$token")"
printf '%s\n' "$prepare" | grep -q '"state":"prepared"'
cmp -s "$guard/$token/expected-final/cake-autorate" "$config/cake-autorate"
cmp -s "$guard/$token/expected-final/sqm" "$config/sqm"
if uci -q show cake-autorate.wan_sqm | grep -q '_autotune_apply_'; then
	echo "prepare-confirm left a persistent CAKE marker" >&2
	exit 1
fi
if uci -q get sqm.cake_autorate_apply_wan_sqm >/dev/null 2>&1; then
	echo "prepare-confirm left an SQM enrollment marker" >&2
	exit 1
fi
$helper reconcile "$token" | grep -q '"state":"confirmed"'
$helper finalize "$token" | grep -q '"state":"finalized"'
[ ! -e "$guard/$token" ]
if uci -q get cake-autorate.wan_sqm._autotune_apply_guard >/dev/null 2>&1; then
	echo "finalize left a persistent apply marker" >&2
	exit 1
fi
if uci -q get sqm.cake_autorate_apply_wan_sqm >/dev/null 2>&1; then
	echo "finalize left an SQM rollback marker" >&2
	exit 1
fi
[ "$(sed -n '1p' "$APPLY_GUARD_SQM_INIT_STATE")" = enabled ]
$helper verify-init | grep -q '"state":"clear"'

# A reviewed-but-disabled existing instance must not require live qdiscs. Init
# removes its default managed queue and the postcheck requires daemon absence.
export APPLY_GUARD_DAEMON_RUNNING=0
disabled_arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 0 0 "$fingerprint")"
disabled_token="$(printf '%s\n' "$disabled_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
cp "$guard/$disabled_token/expected/cake-autorate" "$config/cake-autorate"
uci -q set sqm.cake_autorate_apply_wan_sqm=cake_autorate_apply_guard
uci -q set sqm.cake_autorate_apply_wan_sqm._autotune_apply_guard=1
uci -q set sqm.cake_autorate_apply_wan_sqm._autotune_apply_job=wan_sqm
uci -q set sqm.cake_autorate_apply_wan_sqm._autotune_apply_fingerprint="$fingerprint"
uci -q set sqm.cake_autorate_apply_wan_sqm._autotune_apply_token="$disabled_token"
$helper verify-init >/dev/null
cp "$guard/$disabled_token/expected/sqm" "$config/sqm"
APPLY_GUARD_TC_ACTIVE=0 $helper postcheck "$disabled_token" >/dev/null
$helper prepare-confirm "$disabled_token" >/dev/null
# Simulate loss of all tmpfs proof after successful prepare-confirm. Persistent
# configuration must contain no token-dependent marker and init must stay clear.
rm -rf "$guard/$disabled_token"
$helper verify-init | grep -q '"state":"clear"'
if uci -q show cake-autorate.wan_sqm | grep -q '_autotune_apply_'; then
	echo "tmpfs loss after prepare-confirm exposed a tokenless CAKE marker" >&2
	exit 1
fi
if uci -q get sqm.cake_autorate_apply_wan_sqm >/dev/null 2>&1; then
	echo "tmpfs loss after prepare-confirm exposed a tokenless SQM marker" >&2
	exit 1
fi

# A marker committed by another page has no root-owned token and must fail
# closed rather than letting init stop the known-good SQM state.
uci -q set cake-autorate.wan_sqm._autotune_apply_guard=1
if $helper verify-init >/dev/null 2>&1; then
	echo "tokenless apply marker was accepted" >&2
	exit 1
fi
uci -q delete cake-autorate.wan_sqm._autotune_apply_guard

# A symlinked/foreign runtime root is never trusted.
rmdir "$guard"
ln -s "$work/config" "$guard"
if $helper verify-init >/dev/null 2>&1; then
	echo "symlinked apply-guard root was accepted" >&2
	exit 1
fi
rm "$guard"
mkdir "$guard"

# Looking up a well-formed but nonexistent token must not allocate RAM/inodes.
missing_token=dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd
before_missing="$(find "$guard" -mindepth 1 -maxdepth 1 -type d | wc -l)"
if $helper postcheck "$missing_token" >/dev/null 2>&1; then
	echo "missing apply token unexpectedly passed" >&2
	exit 1
fi
after_missing="$(find "$guard" -mindepth 1 -maxdepth 1 -type d | wc -l)"
[ "$before_missing" = "$after_missing" ] || {
	echo "missing token lookup leaked a directory" >&2
	exit 1
}

# Old malformed, unreferenced arm directories are reclaimed, while capacity
# remains bounded for recent or live transaction state.
orphan_token=eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee
mkdir "$guard/$orphan_token"
touch -d '5 minutes ago' "$guard/$orphan_token"
orphan_replacement="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 0 0 "$fingerprint")"
orphan_replacement_token="$(printf '%s\n' "$orphan_replacement" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
[ ! -e "$guard/$orphan_token" ]
$helper abort "$orphan_replacement_token" >/dev/null

# Bound abandoned tokens, garbage-collect only an expired root-owned token,
# and still refuse a ninth live transaction.
capacity_tokens=
capacity_first=
capacity_index=0
while [ "$capacity_index" -lt 8 ]; do
	capacity_arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 0 0 "$fingerprint")"
	capacity_token="$(printf '%s\n' "$capacity_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
	[ -n "$capacity_token" ]
	[ -n "$capacity_first" ] || capacity_first="$capacity_token"
	capacity_tokens="${capacity_tokens:+$capacity_tokens }$capacity_token"
	capacity_index=$((capacity_index + 1))
done
if $helper arm wan_sqm pppoe-wan speedtest-go main '' 0 0 "$fingerprint" >/dev/null 2>&1; then
	echo "apply guard exceeded its live-token capacity" >&2
	exit 1
fi
sed -i 's/^expires_epoch=.*/expires_epoch=0/' "$guard/$capacity_first/meta"
replacement_arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 0 0 "$fingerprint")"
replacement_token="$(printf '%s\n' "$replacement_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
[ ! -e "$guard/$capacity_first" ]
if $helper arm wan_sqm pppoe-wan speedtest-go main '' 0 0 "$fingerprint" >/dev/null 2>&1; then
	echo "apply guard capacity was not re-enforced after GC" >&2
	exit 1
fi
for capacity_token in $capacity_tokens "$replacement_token"; do
	$helper abort "$capacity_token" >/dev/null
done
[ "$(find "$guard" -mindepth 1 -maxdepth 1 -type d | wc -l)" -eq 0 ]

echo "apply guard tests passed"
