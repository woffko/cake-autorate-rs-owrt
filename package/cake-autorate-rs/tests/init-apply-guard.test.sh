#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
init_script="$base/files/etc/init.d/cake-autorate"
work="${TMPDIR:-/tmp}/cake-init-apply-guard-test.$$"
log="$work/mutations"
helper="$work/apply-guard"
mkdir -p "$work"
trap 'rm -rf "$work"' EXIT INT TERM

cat > "$helper" <<'EOF'
#!/bin/sh
case "${APPLY_GUARD_FIXTURE_STATE:-reject}" in
	verified) printf '%s\n' '{"state":"verified","schema_version":1,"markers":1}' ;;
	clear) printf '%s\n' '{"state":"clear","schema_version":1}' ;;
	*) printf '%s\n' '{"state":"failed","error":"token missing"}'; exit 1 ;;
esac
EOF
chmod +x "$helper"

harness() {
	operation="$1"
	guard_path="$2"
	state="$3"
	markers="$4"
	CAKE_AUTORATE_APPLY_GUARD="$guard_path"
	APPLY_GUARD_FIXTURE_STATE="$state"
	APPLY_GUARD_FIXTURE_MARKERS="$markers"
	export CAKE_AUTORATE_APPLY_GUARD APPLY_GUARD_FIXTURE_STATE APPLY_GUARD_FIXTURE_MARKERS
	. "$init_script"

	logger() { :; }
	uci() {
		[ "${1:-}" = -q ] && [ "${2:-}" = show ] || return 1
		case "${3:-}:${APPLY_GUARD_FIXTURE_MARKERS:-both}" in
			cake-autorate:cake|cake-autorate:both)
				printf '%s\n' "cake-autorate.wan_sqm._autotune_apply_guard='1'"
				;;
			sqm:sqm|sqm:both)
				printf '%s\n' "sqm.cake_autorate_apply_wan_sqm._autotune_apply_guard='1'"
				;;
		esac
	}
	mark() { printf '%s\n' "$1" >> "$log"; }
	service_runtime_lock_acquire_or_exit() { :; }
	service_runtime_lock_release_or_exit() { :; }
	config_load() { mark config-load; }
	sync_interface_presets() { mark sync-presets; }
	detect_managed_sqm_conflicts() { mark conflicts; }
	sync_sqm_config() { mark sync-sqm; }
	prepare_sqm_ingress_interfaces() { mark ingress; }
	start_sqm_backend() { mark start-sqm; }
	stop_managed_sqm_backend() { mark stop-sqm; }
	config_foreach() { mark start-instance; }
	stop() { mark stop-service; }
	start() { mark start-service; }

	case "$operation" in
		start) start_service_locked ;;
		reload) reload_service ;;
		stop)
			stop_service
			# This represents rc.common's unconditional procd_kill after a normal
			# stop_service return. The rejected path must exit before reaching it.
			mark procd-kill
			;;
	esac
}

	case "${1:-}" in
	harness)
		log="$6"
		harness "$2" "$3" "$4" "$5"
		exit $?
		;;
esac

assert_rejected_without_mutation() {
	operation="$1"
	guard_path="$2"
	state="$3"
	markers="${4:-cake}"
	: > "$log"
	if sh "$0" harness "$operation" "$guard_path" "$state" "$markers" "$log" >/dev/null 2>&1; then
		echo "$operation unexpectedly accepted an invalid apply marker" >&2
		exit 1
	fi
	if [ -s "$log" ]; then
		echo "$operation mutated service/SQM before apply-guard rejection" >&2
		cat "$log" >&2
		exit 1
	fi
}

for operation in start reload stop; do
	assert_rejected_without_mutation "$operation" "$helper" reject
	assert_rejected_without_mutation "$operation" "$helper" clear
	assert_rejected_without_mutation "$operation" "$work/missing-helper" verified
	# Package events may expose cake-only on forward apply or SQM-only on
	# cake-first rollback. Both orphan states must fence every mutation.
	assert_rejected_without_mutation "$operation" "$helper" reject cake
	assert_rejected_without_mutation "$operation" "$helper" reject sqm
done

: > "$log"
sh "$0" harness start "$helper" verified both "$log"
grep -qx config-load "$log"
grep -qx sync-sqm "$log"
grep -qx start-sqm "$log"

: > "$log"
sh "$0" harness reload "$helper" verified both "$log"
grep -qx stop-service "$log"
grep -qx start-service "$log"

: > "$log"
sh "$0" harness stop "$helper" verified both "$log"
grep -qx stop-sqm "$log"
grep -qx procd-kill "$log"

echo "init apply guard tests passed"
