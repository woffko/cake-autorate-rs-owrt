#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
repo="$(CDPATH= cd -- "$base/../.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/cake-calibration-init-test.XXXXXX")"
trap 'rm -rf "$work"' EXIT INT TERM
init="$repo/package/cake-autorate-rs/files/etc/init.d/cake-autorate-autotune"
daemon="$work/cake-autorated"
events="$work/events"

cat >"$daemon" <<'EOF'
#!/bin/sh
printf 'daemon:%s\n' "$*" >>"$CALIBRATION_INIT_EVENTS"
case "$*" in
	'--calibration-service prepare-start')
		printf '%b\n' "${CALIBRATION_INIT_PLAN:-cake-autorate-calibration-service\\t1\\tcoordinator}"
		;;
	'--calibration-service confirm-started')
		printf '%b\n' "${CALIBRATION_STARTED_PLAN:-cake-autorate-calibration-service\\t1\\tready}"
		;;
	'--calibration-service prepare-stop')
		[ "${CALIBRATION_STOP_READY_FAIL:-0}" != 1 ] || exit 1
		printf '%b\n' "${CALIBRATION_STOP_READY_PLAN:-cake-autorate-calibration-service\\t1\\tstop-ready}"
		;;
	'--calibration-service confirm-stopped')
		printf '%b\n' "${CALIBRATION_STOP_PLAN:-cake-autorate-calibration-service\\t1\\tstopped}"
		;;
	*) exit 2 ;;
esac
EOF
chmod +x "$daemon"

logger() { :; }
procd_open_instance() { printf 'open:%s\n' "$1" >>"$events"; }
procd_set_param() { printf 'set:%s\n' "$*" >>"$events"; }
procd_close_instance() { printf 'close\n' >>"$events"; }

. "$init"
DAEMON="$daemon"
CALIBRATION_INIT_EVENTS="$events"
export CALIBRATION_INIT_EVENTS

start_service

expected="daemon:--calibration-service prepare-start
open:coordinator
set:command $daemon --calibrationd --native-rating --native-speedtest --native-autotune --native-scheduler --scheduler-store-dir /etc/cake-autorate-rs-scheduler
set:respawn 3600 5 5
set:term_timeout 5
set:stdout 1
set:stderr 1
close"
[ "$(cat "$events")" = "$expected" ] || {
	echo 'calibration init did not register the exact native coordinator plan' >&2
	exit 1
}

: >"$events"
CALIBRATION_INIT_PLAN='cake-autorate-calibration-service\t1\tdeferred'
CALIBRATION_START_DEFERRED=0
export CALIBRATION_INIT_PLAN
start_service
service_started
[ "$(cat "$events")" = 'daemon:--calibration-service prepare-start' ] || {
	echo 'package-upgrade deferral registered or attested a premature coordinator' >&2
	exit 1
}
unset CALIBRATION_INIT_PLAN
CALIBRATION_START_DEFERRED=0

for invalid in \
	'cake-autorate-calibration-service\t1\twrong' \
	'cake-autorate-calibration-service\t2\tcoordinator' \
	'cake-autorate-calibration-service 1 coordinator' \
	'cake-autorate-calibration-service\t1\tcoordinator\nextra'; do
	: >"$events"
	CALIBRATION_INIT_PLAN="$invalid"
	export CALIBRATION_INIT_PLAN
	if start_service >/dev/null 2>&1; then
		echo "calibration init accepted malformed lifecycle plan: $invalid" >&2
		exit 1
	fi
	[ "$(wc -l <"$events")" -eq 1 ] || {
		echo 'malformed lifecycle plan published a partial procd instance' >&2
		exit 1
	}
done

: >"$events"
service_started
[ "$(cat "$events")" = 'daemon:--calibration-service confirm-started' ] || {
	echo 'calibration init did not confirm exact coordinator readiness after procd start' >&2
	exit 1
}

for invalid in \
	'cake-autorate-calibration-service\t1\trunning' \
	'cake-autorate-calibration-service\t2\tready' \
	'cake-autorate-calibration-service 1 ready' \
	'cake-autorate-calibration-service\t1\tready\nextra'; do
	: >"$events"
	CALIBRATION_STARTED_PLAN="$invalid"
	export CALIBRATION_STARTED_PLAN
	if service_started >/dev/null 2>&1; then
		echo "calibration init accepted malformed readiness confirmation: $invalid" >&2
		exit 1
	fi
	[ "$(wc -l <"$events")" -eq 1 ] || {
		echo 'malformed readiness confirmation ran more than the native attestor' >&2
		exit 1
	}
done
unset CALIBRATION_STARTED_PLAN

: >"$events"
stop_service
[ "$(cat "$events")" = 'daemon:--calibration-service prepare-stop' ] || {
	echo 'calibration init did not obtain exact native Apply stop readiness before procd stop' >&2
	exit 1
}

for invalid in \
	'cake-autorate-calibration-service\t1\tbusy' \
	'cake-autorate-calibration-service\t2\tstop-ready' \
	'cake-autorate-calibration-service 1 stop-ready' \
	'cake-autorate-calibration-service\t1\tstop-ready\nextra'; do
	: >"$events"
	CALIBRATION_STOP_READY_PLAN="$invalid"
	export CALIBRATION_STOP_READY_PLAN
	if (stop_service) >/dev/null 2>&1; then
		echo "calibration init accepted malformed stop readiness: $invalid" >&2
		exit 1
	fi
	[ "$(wc -l <"$events")" -eq 1 ] || {
		echo 'malformed stop readiness ran more than the native attestor' >&2
		exit 1
	}
done
unset CALIBRATION_STOP_READY_PLAN

: >"$events"
CALIBRATION_STOP_READY_FAIL=1
export CALIBRATION_STOP_READY_FAIL
if (stop_service) >/dev/null 2>&1; then
	echo 'calibration init ignored a state-driven native Apply stop refusal' >&2
	exit 1
fi
[ "$(cat "$events")" = 'daemon:--calibration-service prepare-stop' ] || {
	echo 'native Apply stop refusal ran more than the exact Rust attestor' >&2
	exit 1
}
unset CALIBRATION_STOP_READY_FAIL

: >"$events"
service_stopped
[ "$(cat "$events")" = 'daemon:--calibration-service confirm-stopped' ] || {
	echo 'calibration init did not confirm exact process exit after procd stop' >&2
	exit 1
}

for invalid in \
	'cake-autorate-calibration-service\t1\trunning' \
	'cake-autorate-calibration-service\t2\tstopped' \
	'cake-autorate-calibration-service 1 stopped' \
	'cake-autorate-calibration-service\t1\tstopped\nextra'; do
	: >"$events"
	CALIBRATION_STOP_PLAN="$invalid"
	export CALIBRATION_STOP_PLAN
	if (service_stopped) >/dev/null 2>&1; then
		echo "calibration init accepted malformed stop confirmation: $invalid" >&2
		exit 1
	fi
	[ "$(wc -l <"$events")" -eq 1 ] || {
		echo 'malformed stop confirmation ran more than the native attestor' >&2
		exit 1
	}
done

if grep -Eq 'config_(load|get)|autotune_scheduler_engine|--native-apply-recover|--legacy-apply-recover|--legacy-autotune-recover|--scheduler-adopt-legacy|--calibration-capabilities|sleep|usleep' "$init"; then
	echo 'calibration init still owns recovery, scheduler, capability, or timer policy' >&2
	exit 1
fi

printf '%s\n' 'native calibration lifecycle bridge tests passed'
