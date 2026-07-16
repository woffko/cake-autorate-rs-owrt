#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
init_script="$base/files/etc/init.d/cake-autorate"
lock_lib="$base/files/usr/libexec/cake-autorate-rs/runtime-lock"

poll_delay() {
	# GNU/coreutils accepts fractional sleeps, while some OpenWrt BusyBox
	# builds do not. Keep the fast host test without making the router gate
	# depend on that optional parser feature.
	sleep 0.05 2>/dev/null || sleep 1
}

holder_main() {
	root="$1"
	ready="$2"
	release="$3"
	CAKE_AUTORATE_RUNTIME_LOCK_ROOT="$root"
	export CAKE_AUTORATE_RUNTIME_LOCK_ROOT
	. "$lock_lib"
	runtime_lock_acquire_global_shared
	: > "$ready"
	while [ ! -e "$release" ]; do
		poll_delay
	done
	runtime_lock_release_global
}

harness_main() {
	root="$1"
	log="$2"
	mode="$3"
	ownership="${4:-standalone}"
	CAKE_AUTORATE_RUNTIME_LOCK_LIB="$lock_lib"
	CAKE_AUTORATE_RUNTIME_LOCK_ROOT="$root"
	export CAKE_AUTORATE_RUNTIME_LOCK_LIB CAKE_AUTORATE_RUNTIME_LOCK_ROOT
	. "$init_script"

	assert_exclusive_lock() {
		if sh -c '
			exec 8>&-
			exec 7>>"$1/runtime.guard"
			flock -sn 7
		' sh "$root"; then
			echo "service mutation escaped the global exclusive lock" >&2
			exit 91
		fi
	}

	mark_mutation() {
		assert_exclusive_lock
		printf '%s\n' "$1" >> "$log"
	}

	logger() { :; }
	sleep() { :; }
	config_load() { mark_mutation config-load; }
	sync_interface_presets() { mark_mutation sync-presets; }
	detect_managed_sqm_conflicts() { mark_mutation detect-conflicts; }
	sync_sqm_config() { mark_mutation sync-sqm; }
	prepare_sqm_ingress_interfaces() { mark_mutation prepare-ingress; }
	start_sqm_backend() { mark_mutation start-sqm; }
	stop_managed_sqm_backend() { mark_mutation stop-sqm; }
	cleanup_runtime_files() { mark_mutation cleanup-runtime; }
	config_foreach() { mark_mutation start-instances; }

	stop() {
		stop_service "$@"
		mark_mutation procd-kill
		service_stopped
	}

	start() {
		case "$mode" in
			reload|restart|recover)
				# Verify service_stopped() did not release the transaction before
				# start_service() begins.
				mark_mutation stop-start-boundary
				;;
		esac
		start_service "$@"
	}

	uci() {
		if [ "${1:-}" = -q ] && [ "${2:-}" = show ] && [ "${3:-}" = cake-autorate ]; then
			printf '%s\n' "cake-autorate.wan=cake_autorate"
			return 0
		fi
		return 1
	}

	config_get() {
		variable="$1"
		section="$2"
		option="$3"
		fallback="${4:-}"
		value="$fallback"
		case "$section.$option" in
			wan.mwan3_member) value=wan ;;
		esac
		eval "$variable=\$value"
	}

	config_get_bool() {
		variable="$1"
		section="$2"
		option="$3"
		fallback="${4:-0}"
		value="$fallback"
		case "$section.$option" in
			wan.enabled) value=1 ;;
		esac
		eval "$variable=\$value"
	}

	pgrep() { return 1; }

	case "$mode" in
		start) start_service ;;
		stop) stop ;;
		reload) reload_service ;;
		restart) restart ;;
		recover) recover_interface wan ;;
		*) echo "unknown harness mode: $mode" >&2; return 2 ;;
	esac

	# A standalone lifecycle releases its own lock.  A borrowed lifecycle must
	# leave the scheduler-owned descriptor continuously exclusive.
	exec 7>>"$root/runtime.guard"
	if [ "$ownership" = borrowed ]; then
		if flock -sn 7; then
			flock -u 7
			exec 7>&-
			echo "borrowed lifecycle released its parent's lock" >&2
			exit 92
		fi
	else
		flock -sn 7
		flock -u 7
	fi
	exec 7>&-
}

borrowed_main() {
	root="$1"
	log="$2"
	mode="$3"
	CAKE_AUTORATE_RUNTIME_LOCK_ROOT="$root"
	export CAKE_AUTORATE_RUNTIME_LOCK_ROOT
	. "$lock_lib"
	runtime_lock_acquire_global_exclusive
	CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_BORROW=1
	export CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_BORROW
	sh "$0" harness "$root" "$log" "$mode" borrowed

	# The init child exited, but its release path must not have unlocked or
	# closed the parent's open-file-description lock.
	if sh -c '
		exec 8>&-
		exec 7>>"$1/runtime.guard"
		flock -sn 7
	' sh "$root"; then
		echo "borrowed init child released the scheduler-owned lock" >&2
		exit 93
	fi
	runtime_lock_release_global
	sh -c '
		exec 8>&-
		exec 7>>"$1/runtime.guard"
		flock -sn 7
		flock -u 7
	' sh "$root"
}

case "${1:-}" in
	holder)
		holder_main "$2" "$3" "$4"
		exit $?
		;;
	harness)
		harness_main "$2" "$3" "$4" "${5:-standalone}"
		exit $?
		;;
	borrowed)
		borrowed_main "$2" "$3" "$4"
		exit $?
		;;
esac

work="${TMPDIR:-/tmp}/cake-init-runtime-lock-test.$$"
root="$work/locks"
ready="$work/holder.ready"
release="$work/holder.release"
log="$work/mutations.log"
holder_pid=""
mkdir -p "$work"
cleanup() {
	[ -z "$holder_pid" ] || kill "$holder_pid" 2>/dev/null || true
	[ -z "$holder_pid" ] || wait "$holder_pid" 2>/dev/null || true
	rm -rf "$work"
}
trap cleanup EXIT INT TERM

sh "$0" holder "$root" "$ready" "$release" &
holder_pid=$!
i=0
while [ ! -e "$ready" ]; do
	if ! kill -0 "$holder_pid" 2>/dev/null; then
		echo "shared-lock holder exited before becoming ready" >&2
		exit 1
	fi
	i=$((i + 1))
	[ "$i" -lt 100 ] || {
		echo "timed out waiting for shared-lock holder" >&2
		exit 1
	}
	poll_delay
done

# Every public lifecycle entry point must fail before its first mutation while
# an Auto-Tune-style shared lock is active.  In particular, stop must exit
# before the rc.common wrapper reaches procd_kill().
for mode in start stop reload restart recover; do
	if sh "$0" harness "$root" "$log" "$mode" >/dev/null 2>&1; then
		echo "$mode unexpectedly ran while the shared runtime lock was held" >&2
		exit 1
	fi
	[ ! -s "$log" ] || {
		echo "$mode mutated service or SQM state before reporting a busy lock" >&2
		exit 1
	}
done

: > "$release"
wait "$holder_pid"
holder_pid=""

sh "$0" harness "$root" "$log" reload

expected="stop-sqm
procd-kill
cleanup-runtime
stop-start-boundary
config-load
sync-presets
detect-conflicts
sync-sqm
prepare-ingress
start-sqm
start-instances"
actual="$(cat "$log")"
[ "$actual" = "$expected" ] || {
	echo "reload did not complete as one healthy stop/start transaction" >&2
	printf 'expected:\n%s\nactual:\n%s\n' "$expected" "$actual" >&2
	exit 1
}

: > "$log"
sh "$0" borrowed "$root" "$log" restart
actual="$(cat "$log")"
[ "$actual" = "$expected" ] || {
	echo "borrowed restart did not preserve one continuous lifecycle transaction" >&2
	printf 'expected:\n%s\nactual:\n%s\n' "$expected" "$actual" >&2
	exit 1
}

echo "init runtime-lock lifecycle tests passed"
