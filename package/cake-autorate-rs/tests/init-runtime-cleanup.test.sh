#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
init_script="$base/files/etc/init.d/cake-autorate"
lock_lib="$base/files/usr/libexec/cake-autorate-rs/runtime-lock"
work="$(mktemp -d "${TMPDIR:-/tmp}/cake-init-runtime-cleanup-test.XXXXXX")"
runtime="$work/run"
outside="$work/outside"
log="$work/actions.log"

cleanup() {
	rm -rf "$work"
}
trap cleanup EXIT INT TERM

mkdir -p "$runtime/live" "$runtime/dead" "$runtime/bad-name" "$outside"
for name in status.json history.csv history.csv.tmp; do
	: > "$runtime/live/$name"
	: > "$runtime/dead/$name"
	: > "$runtime/bad-name/$name"
	: > "$outside/$name"
done
ln -s "$outside" "$runtime/linked"

CAKE_AUTORATE_RUNTIME_LOCK_LIB="$lock_lib"
CAKE_AUTORATE_RUN_ROOT="$runtime"
export CAKE_AUTORATE_RUNTIME_LOCK_LIB CAKE_AUTORATE_RUN_ROOT
. "$init_script"

logger() { :; }
pgrep() {
	case "$*" in
		*'--instance live$'*) return 0 ;;
		*) return 1 ;;
	esac
}

cleanup_runtime_files

[ -f "$runtime/live/status.json" ]
[ -f "$runtime/live/history.csv" ]
[ ! -e "$runtime/dead/status.json" ]
[ ! -e "$runtime/dead/history.csv" ]
[ ! -e "$runtime/dead/history.csv.tmp" ]
[ -f "$runtime/bad-name/status.json" ]
[ -f "$outside/status.json" ]

# A detached/asynchronous service_stopped callback must acquire the global
# lifecycle lock before it invokes cleanup, then release only the lock it
# acquired itself.
sleep() { :; }
service_runtime_lock_acquire_or_exit() {
	[ "$SERVICE_RUNTIME_LOCK_HELD" -eq 0 ]
	SERVICE_RUNTIME_LOCK_HELD=1
	printf '%s\n' acquire >> "$log"
}
service_runtime_lock_release_or_exit() {
	[ "$SERVICE_RUNTIME_LOCK_HELD" -eq 1 ]
	SERVICE_RUNTIME_LOCK_HELD=0
	printf '%s\n' release >> "$log"
}
cleanup_runtime_files() {
	[ "$SERVICE_RUNTIME_LOCK_HELD" -eq 1 ]
	printf '%s\n' cleanup >> "$log"
}

SERVICE_RUNTIME_LOCK_HELD=0
SERVICE_RUNTIME_STOP_RELEASE_PENDING=0
service_stopped
[ "$(cat "$log")" = "acquire
cleanup
release" ]
[ "$SERVICE_RUNTIME_LOCK_HELD" -eq 0 ]

printf '%s\n' 'init runtime cleanup tests passed'
