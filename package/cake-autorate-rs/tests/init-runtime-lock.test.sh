#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
init_template="$base/files/etc/init.d/cake-autorate"
init_renderer="$base/scripts/render-init-variant.sh"
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
	mkdir -p "$root"
	chmod 700 "$root"
	exec 8>>"$root/runtime.guard"
	chmod 600 "$root/runtime.guard"
	flock -s 8
	: > "$ready"
	while [ ! -e "$release" ]; do
		poll_delay
	done
	flock -u 8
	exec 8>&-
}

harness_main() {
	root="$1"
	log="$2"
	mode="$3"
	ownership="${4:-standalone}"
	CAKE_AUTORATE_RUNTIME_LOCK_LIB="$lock_lib"
	CAKE_AUTORATE_RUNTIME_LOCK_ROOT="$root"
	export CAKE_AUTORATE_RUNTIME_LOCK_LIB CAKE_AUTORATE_RUNTIME_LOCK_ROOT
	init_script="$root/cake-autorate.full.$$"
	sh "$init_renderer" full "$init_template" "$init_script"
	. "$init_script"
	rm -f "$init_script"
	DAEMON="$root/cake-autorated"
	CAKE_TEST_LIFECYCLE_LOG="$log"
	export CAKE_TEST_LIFECYCLE_LOG
	cat >"$DAEMON" <<'EOF'
#!/bin/sh
case "$*" in
	'--service-lifecycle prepare-start')
		if [ -e "$CAKE_AUTORATE_RUNTIME_LOCK_ROOT/native-apply-recovery/current" ]; then
			exit 70
		fi
		if [ "${PKG_UPGRADE:-0}" = 1 ]; then
			printf '%s\n' 'service-start-v2 - -'
		else
			printf '%s\n' native-prepare-start >> "$CAKE_TEST_LIFECYCLE_LOG"
			printf '%s\n' 'service-start-v2 wan -'
		fi
		;;
	'--service-lifecycle execute-stop')
		[ "${CAKE_AUTORATE_SERVICE_LOCK_BORROW:-}" = 1 ]
		[ "${CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_FD:-}" = 8 ]
		printf '%s\n' native-execute-stop >> "$CAKE_TEST_LIFECYCLE_LOG"
		printf '%s\n' 'service-stop-v1 ok'
		;;
	*) exit 64 ;;
esac
EOF
	chmod +x "$DAEMON"

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
	procd_open_instance() { mark_mutation "procd-open:$1"; }
	procd_set_param() { mark_mutation "procd-set:$1"; }
	procd_close_instance() { mark_mutation procd-close; }

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

	case "$mode" in
		start) start_service ;;
		upgrade-start)
			PKG_UPGRADE=1
			export PKG_UPGRADE
			start_service
			[ ! -s "$log" ] || {
				echo "default_postinst upgrade start mutated runtime state" >&2
				return 94
			}
			;;
		stop) stop ;;
		reload) reload_service ;;
		restart) restart ;;
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

work="$(mktemp -d "${TMPDIR:-/tmp}/cake-init-runtime-lock-test.XXXXXX")"
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
for mode in start stop reload restart; do
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

: > "$log"
sh "$0" harness "$root" "$log" upgrade-start
[ ! -s "$log" ] || {
	echo "upgrade start guard did not remain side-effect free" >&2
	exit 1
}

# A durable native-Apply transaction must stop the ordinary service before it
# mutates SQM. Rust owns this marker policy; the rc.common bridge must not
# contain a second filesystem check or an environment bypass.
mkdir -p "$root/native-apply-recovery"
: > "$root/native-apply-recovery/current"
: > "$log"
if sh "$0" harness "$root" "$log" start >/dev/null 2>&1; then
	echo "ordinary start ignored a pending native Apply recovery transaction" >&2
	exit 1
fi
[ ! -s "$log" ] || {
	echo "ordinary start mutated runtime state before the native Apply recovery guard" >&2
	exit 1
}

if CAKE_AUTORATE_NATIVE_APPLY_RECOVERY=1 \
	sh "$0" harness "$root" "$log" start >/dev/null 2>&1; then
	echo "ordinary start accepted a native Apply marker through a shell-era bypass" >&2
	exit 1
fi
[ ! -s "$log" ] || {
	echo "native Apply marker bypass mutated runtime state" >&2
	exit 1
}

rm -f "$root/native-apply-recovery/current"
sh "$0" harness "$root" "$log" start
expected_start="native-prepare-start
procd-open:wan
procd-set:command
procd-set:respawn
procd-set:stdout
procd-set:stderr
procd-close"
actual="$(cat "$log")"
[ "$actual" = "$expected_start" ] || {
	echo "native Apply recovery owner could not use the guarded init path" >&2
	printf 'expected:\n%s\nactual:\n%s\n' "$expected_start" "$actual" >&2
	exit 1
}
rm -f "$root/native-apply-recovery/current"
: > "$log"

sh "$0" harness "$root" "$log" reload

expected="native-execute-stop
procd-kill
stop-start-boundary
native-prepare-start
procd-open:wan
procd-set:command
procd-set:respawn
procd-set:stdout
procd-set:stderr
procd-close"
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
