#!/bin/sh
set -eu

ROOT="$(mktemp -d)"
trap 'rm -rf "$ROOT"' EXIT
mkdir -p "$ROOT/bin" "$ROOT/sys/eth0" "$ROOT/proc" "$ROOT/runtime" \
	"$ROOT/autotune" "$ROOT/quality" "$ROOT/speedtest" "$ROOT/apply"

cat > "$ROOT/bin/uci" <<'EOF'
#!/bin/sh
case "$*" in
	"-q show cake-autorate")
		printf '%s\n' "cake-autorate.wan_sqm=cake_autorate"
		;;
	"-q show cake-autorate.wan_sqm")
		printf '%s\n' "cake-autorate.wan_sqm=cake_autorate"
		if [ "${RH_MARKER:-0}" = 1 ]; then
			printf '%s\n' \
				"cake-autorate.wan_sqm._autotune_apply_guard='1'" \
				"cake-autorate.wan_sqm._autotune_apply_token='${RH_TOKEN:-}'"
		fi
		;;
	"-q get cake-autorate.wan_sqm.enabled") printf '%s\n' "${RH_ENABLED:-1}" ;;
	"-q get cake-autorate.wan_sqm.manage_sqm") printf '%s\n' "${RH_MANAGE:-1}" ;;
	"-q get cake-autorate.wan_sqm.sqm_enabled") printf '%s\n' "${RH_SQM_ENABLED:-1}" ;;
	"-q get cake-autorate.wan_sqm.traffic_rules_enabled") printf '%s\n' "${RH_RULES_ENABLED-1}" ;;
	"-q get cake-autorate.wan_sqm.autotune_profile") printf '%s\n' "${RH_PROFILE:-best_overall}" ;;
	"-q get cake-autorate.wan_sqm.wan_if") printf 'eth0\n' ;;
	"-q get cake-autorate.wan_sqm.sqm_interface") printf 'eth0\n' ;;
	"-q get cake-autorate.wan_sqm.ul_if") printf 'eth0\n' ;;
	"-q get cake-autorate.wan_sqm.dl_if") printf 'ifb4eth0\n' ;;
	"-q get cake-autorate.wan_sqm.sqm_section") printf 'cake_wan_sqm\n' ;;
	"-q get cake-autorate.wan_sqm._autotune_apply_guard")
		[ "${RH_MARKER:-0}" = 1 ] && printf '1\n' || exit 1
		;;
	"-q get cake-autorate.wan_sqm._autotune_apply_token")
		[ "${RH_MARKER:-0}" = 1 ] && printf '%s\n' "${RH_TOKEN:-}" || exit 1
		;;
	"-q get sqm.cake_wan_sqm")
		[ "${RH_QUEUE_PRESENT:-1}" = 1 ] && printf 'queue\n' || exit 1
		;;
	"-q get sqm.cake_wan_sqm._cake_autorate_managed")
		printf '%s\n' "${RH_OWNER:-wan_sqm}"
		;;
	"-q get sqm.cake_wan_sqm.interface") printf '%s\n' "${RH_TARGET:-eth0}" ;;
	"-q get sqm.cake_wan_sqm.enabled") printf '%s\n' "${RH_QUEUE_ENABLED:-1}" ;;
	*) exit 1 ;;
esac
EOF

cat > "$ROOT/bin/tc" <<'EOF'
#!/bin/sh
case "${RH_TC_MODE:-healthy}:$*" in
	healthy:"qdisc show dev eth0")
		printf '%s\n' \
			"qdisc cake 8001: root refcnt 2 bandwidth 100Mbit diffserv4" \
			"qdisc ingress ffff: parent ffff:fff1"
		;;
	healthy:"qdisc show dev ifb4eth0")
		printf '%s\n' "qdisc cake 8002: root refcnt 2 bandwidth 500Mbit besteffort"
		;;
	healthy:"filter show dev eth0 ingress")
		printf '%s\n' "action order 1: mirred (Egress Redirect to device ifb4eth0) stolen"
		;;
	missing_dl:"qdisc show dev eth0")
		printf '%s\n' \
			"qdisc cake 8001: root refcnt 2 bandwidth 100Mbit diffserv4" \
			"qdisc ingress ffff: parent ffff:fff1"
		;;
	missing_dl:"filter show dev eth0 ingress")
		printf '%s\n' "action order 1: mirred (Egress Redirect to device ifb4eth0) stolen"
		;;
	*) : ;;
esac
EOF

cat > "$ROOT/bin/traffic-classifier" <<'EOF'
#!/bin/sh
case "${RH_CLASSIFIER_STATE:-active}" in
	active)
		if [ "$*" = "status wan_sqm" ]; then
			printf '%s\n' \
				"{\"state\":\"active\",\"table_present\":true,\"instance\":\"wan_sqm\",\"target\":\"${RH_CLASSIFIER_TARGET:-eth0}\",\"profile\":\"${RH_CLASSIFIER_PROFILE:-best_overall}\"}"
		else
			printf '%s\n' \
				"{\"state\":\"active\",\"table_present\":true,\"attested_instances\":\"wan_sqm|${RH_CLASSIFIER_TARGET:-eth0}|${RH_CLASSIFIER_PROFILE:-best_overall}\"}"
		fi
		;;
	inactive) printf '%s\n' '{"state":"inactive","table_present":false}' ;;
	drifted) printf '%s\n' '{"state":"drifted","table_present":true}' ;;
	untracked) printf '%s\n' '{"state":"untracked","table_present":true}' ;;
	*) exit 1 ;;
esac
EOF

cat > "$ROOT/bin/pgrep" <<'EOF'
#!/bin/sh
[ "$1" = "-f" ] || exit 2
[ "$2" = '^/usr/sbin/cake-autorated --instance wan_sqm$' ] || exit 1
count="${RH_DAEMON_COUNT:-1}"
index=0
while [ "$index" -lt "$count" ]; do
	printf '%s\n' $((1000 + index))
	index=$((index + 1))
done
EOF

chmod +x "$ROOT/bin/uci" "$ROOT/bin/tc" "$ROOT/bin/pgrep" "$ROOT/bin/traffic-classifier"

HELPER="$(dirname "$0")/../root/usr/libexec/cake-autorate-rs/runtime-health"
export CAKE_AUTORATE_UCI_BIN="$ROOT/bin/uci"
export CAKE_AUTORATE_TC_BIN="$ROOT/bin/tc"
export CAKE_AUTORATE_PGREP_BIN="$ROOT/bin/pgrep"
export CAKE_AUTORATE_UBUS_BIN="/bin/false"
export CAKE_AUTORATE_JSONFILTER_BIN="/bin/false"
export CAKE_AUTORATE_SYS_CLASS_NET="$ROOT/sys"
export CAKE_AUTORATE_PROC_ROOT="$ROOT/proc"
export CAKE_AUTORATE_RUNTIME_ROOT="$ROOT/runtime"
export CAKE_AUTORATE_AUTOTUNE_DIR="$ROOT/autotune"
export CAKE_AUTORATE_QUALITY_DIR="$ROOT/quality"
export CAKE_AUTORATE_SPEEDTEST_JOB_DIR="$ROOT/speedtest"
export CAKE_AUTORATE_APPLY_GUARD_DIR="$ROOT/apply"
export CAKE_AUTORATE_TRAFFIC_CLASSIFIER="$ROOT/bin/traffic-classifier"
export RH_TOKEN="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

assert_field() {
	file="$1"
	field="$2"
	expected="$3"
	node - "$file" "$field" "$expected" <<'EOF'
const fs = require('node:fs');
const [file, field, expected] = process.argv.slice(2);
const value = field.split('.').reduce((current, key) => current[key],
	JSON.parse(fs.readFileSync(file, 'utf8')).instances.wan_sqm);
if (String(value) !== expected)
	throw new Error(`${field}: expected ${expected}, received ${String(value)}`);
EOF
}

mkdir -p "$ROOT/sys/ifb4eth0"
export RH_ENABLED=1 RH_MANAGE=1 RH_SQM_ENABLED=1 RH_QUEUE_PRESENT=1
export RH_QUEUE_ENABLED=1 RH_OWNER=wan_sqm RH_TARGET=eth0
export RH_DAEMON_COUNT=1 RH_TC_MODE=healthy RH_MARKER=0
export RH_RULES_ENABLED=1 RH_CLASSIFIER_STATE=active RH_PROFILE=best_overall
"$HELPER" > "$ROOT/healthy.json"
assert_field "$ROOT/healthy.json" overall_state HEALTHY
assert_field "$ROOT/healthy.json" autorate_state RUNNING
assert_field "$ROOT/healthy.json" sqm_config_state ENABLED
assert_field "$ROOT/healthy.json" cake_ul_state ACTIVE
assert_field "$ROOT/healthy.json" cake_ul_rate_kbps 100000
assert_field "$ROOT/healthy.json" cake_dl_state ACTIVE
assert_field "$ROOT/healthy.json" cake_dl_rate_kbps 500000
assert_field "$ROOT/healthy.json" ingress_state ACTIVE
assert_field "$ROOT/healthy.json" classifier_state ACTIVE
assert_field "$ROOT/healthy.json" classifier_profile best_overall
assert_field "$ROOT/healthy.json" cake_ul_mode diffserv4

export RH_RULES_ENABLED='' RH_CLASSIFIER_STATE=inactive
"$HELPER" > "$ROOT/rules-opt-in.json"
assert_field "$ROOT/rules-opt-in.json" overall_state HEALTHY
assert_field "$ROOT/rules-opt-in.json" classifier_state DISABLED
export RH_RULES_ENABLED=1 RH_CLASSIFIER_STATE=active

export RH_ENABLED=0 RH_SQM_ENABLED=0 RH_QUEUE_ENABLED=0 RH_DAEMON_COUNT=0
export RH_TC_MODE=healthy
"$HELPER" > "$ROOT/orphaned.json"
assert_field "$ROOT/orphaned.json" overall_state ORPHANED
assert_field "$ROOT/orphaned.json" autorate_state DISABLED
assert_field "$ROOT/orphaned.json" cake_ul_state ORPHANED
assert_field "$ROOT/orphaned.json" cake_dl_state ORPHANED
assert_field "$ROOT/orphaned.json" ifb_state ORPHANED
assert_field "$ROOT/orphaned.json" ingress_state ORPHANED
assert_field "$ROOT/orphaned.json" classifier_state ORPHANED

rm -rf "$ROOT/sys/ifb4eth0"
export RH_TC_MODE=none RH_CLASSIFIER_STATE=inactive
"$HELPER" > "$ROOT/disabled.json"
assert_field "$ROOT/disabled.json" overall_state DISABLED
assert_field "$ROOT/disabled.json" cake_ul_state ABSENT
assert_field "$ROOT/disabled.json" cake_dl_state ABSENT
assert_field "$ROOT/disabled.json" ingress_state ABSENT

export RH_ENABLED=1 RH_SQM_ENABLED=1 RH_QUEUE_ENABLED=1 RH_DAEMON_COUNT=0
export RH_TC_MODE=missing_dl
"$HELPER" > "$ROOT/degraded.json"
assert_field "$ROOT/degraded.json" overall_state DEGRADED
assert_field "$ROOT/degraded.json" autorate_state STOPPED
assert_field "$ROOT/degraded.json" cake_ul_state ACTIVE
assert_field "$ROOT/degraded.json" cake_dl_state MISSING
assert_field "$ROOT/degraded.json" ifb_state MISSING

mkdir -p "$ROOT/sys/ifb4eth0"
export RH_DAEMON_COUNT=1 RH_TC_MODE=healthy RH_CLASSIFIER_STATE=inactive
"$HELPER" > "$ROOT/missing-classifier.json"
assert_field "$ROOT/missing-classifier.json" overall_state DEGRADED
assert_field "$ROOT/missing-classifier.json" classifier_state MISSING

export RH_CLASSIFIER_STATE=active RH_CLASSIFIER_TARGET=eth9
"$HELPER" > "$ROOT/drifted-classifier.json"
assert_field "$ROOT/drifted-classifier.json" overall_state DEGRADED
assert_field "$ROOT/drifted-classifier.json" classifier_state DRIFTED
export RH_CLASSIFIER_TARGET=eth0

export RH_ENABLED=0 RH_SQM_ENABLED=0 RH_QUEUE_ENABLED=0 RH_TC_MODE=none
export RH_DAEMON_COUNT=0 RH_MARKER=1 RH_CLASSIFIER_STATE=inactive
"$HELPER" > "$ROOT/stale.json"
assert_field "$ROOT/stale.json" overall_state BLOCKED
assert_field "$ROOT/stale.json" apply_state STALE
assert_field "$ROOT/stale.json" operation_state STALE

echo "runtime-health helper tests passed"
