#!/bin/sh
set -eu

test_dir="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"
package_dir="$(CDPATH= cd -- "$test_dir/.." && pwd)"
renderer="$package_dir/scripts/render-init-variant.sh"
template="$package_dir/files/etc/init.d/cake-autorate"
work="$(mktemp -d "${TMPDIR:-/tmp}/cake-init-variant-test.XXXXXX")"
trap 'rm -rf "$work"' EXIT HUP INT TERM

fail() {
	printf '%s\n' "$*" >&2
	exit 1
}

render() {
	sh "$renderer" "$1" "$template" "$2"
}

render full "$work/full"
render lite "$work/lite"
render full "$work/full-second"
render lite "$work/lite-second"

sh -n "$work/full"
sh -n "$work/lite"
cmp -s "$work/full" "$work/full-second" || fail 'Full init rendering is not deterministic'
cmp -s "$work/lite" "$work/lite-second" || fail 'Lite init rendering is not deterministic'
[ -x "$work/full" ] || fail 'Full rendered init is not executable'
[ -x "$work/lite" ] || fail 'Lite rendered init is not executable'

if grep -q '@variant' "$work/full" "$work/lite"; then
	fail 'rendered init contains an internal variant marker'
fi

for rendered in "$work/full" "$work/lite"; do
	duplicates="$(sed -n 's/^\([a-zA-Z_][a-zA-Z0-9_]*\)() {.*/\1/p' \
		"$rendered" | sort | uniq -d)"
	[ -z "$duplicates" ] ||
		fail "rendered init contains duplicate function definitions: $duplicates"
done

grep -q 'DAEMON="/usr/sbin/cake-autorated"' "$work/lite" ||
	fail 'Lite init lost the manual controller daemon'
grep -q -- '"$DAEMON" --service-lifecycle prepare-start' "$work/lite" ||
	fail 'Lite init lost the native service lifecycle bridge'
if grep -Eq '^sync_sqm_(config|instance)\(\)|^start_instance\(\)|^start_sqm_backend\(\)' "$work/lite"; then
	fail 'Lite init still contains retired shell start or SQM projection authority'
fi
grep -q -- '"$DAEMON" --service-lifecycle execute-stop' "$work/lite" ||
	fail 'Lite init lost the native service-stop transaction'
if grep -Eq 'recover_interface|procd_add_interface_trigger|sleep 1|cleanup_runtime_files|stop_managed_sqm_backend' "$work/lite"; then
	fail 'Lite init still contains retired stop, cleanup, timer, or member-recovery authority'
fi
grep -q '/usr/libexec/cake-autorate-rs/runtime-lock' "$work/full" ||
	fail 'Full init lost runtime ownership coordination'
grep -q 'CAKE_AUTORATE_SERVICE_LOCK_BORROW=1' "$work/full" ||
	fail 'Full init lost the exact borrowed Rust service lifecycle authority'
if grep -Eq 'service_legacy_apply_markers_present|service_legacy_apply_recovery_gate|\._autotune_apply_' "$work/full"; then
	fail 'Full init still owns retired Apply marker policy in shell'
fi
if grep -q '/usr/libexec/cake-autorate-rs/apply-guard' "$work/full"; then
	fail 'Full init still references the retired shell Apply Guard'
fi
if grep -q -- '--traffic-classifier clear' "$work/full"; then
	fail 'Full init still clears traffic classification in shell'
fi
grep -q '^native_apply_register_instance() {' "$work/full" ||
	fail 'Full init lost the native Apply procd registration bridge'
grep -q -- '"$DAEMON" --service-lifecycle prepare-start' "$work/full" ||
	fail 'Full init lost the native ordinary service lifecycle bridge'
grep -q -- '"$DAEMON" --service-lifecycle execute-stop' "$work/full" ||
	fail 'Full init lost the native service-stop transaction'
grep -q -- '"$DAEMON" --service-lifecycle confirm-started' "$work/full" ||
	fail 'Full init lost post-lock controller readiness confirmation'
if grep -q -- '--service-lifecycle confirm-started' "$work/lite"; then
	fail 'Lite init gained the Full-only native Apply readiness surface'
fi
if grep -Eq 'recover_interface|procd_add_interface_trigger|sleep 1|cleanup_runtime_files|stop_managed_sqm_backend' "$work/full"; then
	fail 'Full init still contains retired stop, cleanup, timer, or member-recovery authority'
fi
if grep -Eq '^native_apply_(restart|stop)_instance\(\)|^native_apply_stop_selected' "$work/full"; then
	fail 'Full init still contains retired selected-instance lifecycle authority'
fi
grep -q 'procd_set_param command "$DAEMON" --mqtt-publisher "$1"' "$work/full" ||
	fail 'Full init lost MQTT procd lifecycle integration'
if grep -q '/etc/init.d/cake-autorate-mqtt' "$work/full"; then
	fail 'Full init still delegates MQTT lifecycle to a second init service'
fi

for forbidden in \
	'autotune' \
	'Auto-Tune' \
	'calibration' \
	'rating' \
	'speed test' \
	'speedtest' \
	'native_apply' \
	'apply-guard' \
	'runtime-lock' \
	'mqtt' \
	'traffic-classifier'
do
	if grep -Fqi "$forbidden" "$work/lite"; then
		fail "Lite init contains forbidden Full surface: $forbidden"
	fi
done

if sh "$renderer" invalid "$template" "$work/invalid" >/dev/null 2>&1; then
	fail 'renderer accepted an unknown variant'
fi
[ ! -e "$work/invalid" ] || fail 'unknown variant left an output file'

ln -s "$template" "$work/template-link"
if sh "$renderer" lite "$work/template-link" "$work/symlink-output" >/dev/null 2>&1; then
	fail 'renderer accepted a symlink template'
fi
[ ! -e "$work/symlink-output" ] || fail 'symlink rejection left an output file'

cat >"$work/nested" <<'EOF'
#!/bin/sh
# @variant full begin
# @variant lite begin
# @variant lite end
# @variant full end
EOF
if sh "$renderer" lite "$work/nested" "$work/nested-output" >/dev/null 2>&1; then
	fail 'renderer accepted nested variant markers'
fi
[ ! -e "$work/nested-output" ] || fail 'nested-marker rejection left an output file'

cat >"$work/mismatched" <<'EOF'
#!/bin/sh
# @variant full begin
# @variant lite end
EOF
if sh "$renderer" lite "$work/mismatched" "$work/mismatched-output" >/dev/null 2>&1; then
	fail 'renderer accepted mismatched variant markers'
fi
[ ! -e "$work/mismatched-output" ] || fail 'mismatched-marker rejection left an output file'

cat >"$work/unterminated" <<'EOF'
#!/bin/sh
# @variant lite begin
true
EOF
if sh "$renderer" lite "$work/unterminated" "$work/unterminated-output" >/dev/null 2>&1; then
	fail 'renderer accepted an unterminated variant section'
fi
[ ! -e "$work/unterminated-output" ] || fail 'unterminated-marker rejection left an output file'

printf '%s\n' 'init variant tests passed'
