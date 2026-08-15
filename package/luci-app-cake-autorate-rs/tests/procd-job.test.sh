#!/bin/sh
set -eu

test_dir="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"
helper="$test_dir/../root/usr/libexec/cake-autorate-rs/procd-job"
work="$(mktemp -d "${TMPDIR:-/tmp}/cake-procd-job-test.XXXXXX")"
worker_pid=""

cleanup() {
	[ -z "$worker_pid" ] || kill "$worker_pid" 2>/dev/null || true
	rm -rf "$work" /tmp/cake-autorate-speedtest/procd-job-test.log \
		/tmp/cake-autorate-quality/procd-job-test
}
trap cleanup EXIT INT TERM

mkdir -p "$work/bin" /tmp/cake-autorate-speedtest \
	/tmp/cake-autorate-quality/procd-job-test
cat > "$work/bin/test-worker" <<'EOF'
#!/bin/sh
printf '%s\n' "$*"
EOF
chmod +x "$work/bin/test-worker"

cat > "$work/bin/ubus" <<'EOF'
#!/bin/sh
work="$CAKE_PROCD_TEST_WORK"
operation="$3"
case "$operation" in
	delete)
		if [ -s "$work/pid" ]; then
			pid="$(sed -n '1p' "$work/pid")"
			kill "$pid" 2>/dev/null || true
			rm -f "$work/pid"
		fi
		printf '{}\n'
		;;
	add)
		setsid /bin/sleep 30 </dev/null >/dev/null 2>&1 &
		printf '%s\n' "$!" > "$work/pid"
		printf '{}\n'
		;;
	list)
		pid="$(sed -n '1p' "$work/pid")"
		printf '{"cake-autorate-test":{"instances":{"worker":{"pid":%s}}}}\n' "$pid"
		;;
	*) exit 1 ;;
esac
EOF
chmod +x "$work/bin/ubus"

cat > "$work/bin/jsonfilter" <<'EOF'
#!/bin/sh
expression=""
while [ "$#" -gt 0 ]; do
	case "$1" in -e) expression="$2"; shift 2 ;; *) shift ;; esac
done
input="$(sed -n '1p')"
case "$expression" in
	*'.pid') printf '%s\n' "$input" | sed -n 's/.*"pid":\([0-9][0-9]*\).*/\1/p' ;;
	*) exit 1 ;;
esac
EOF
chmod +x "$work/bin/jsonfilter"

export CAKE_PROCD_TEST_WORK="$work"
export CAKE_AUTORATE_PROCD_JOB_UBUS="$work/bin/ubus"
export CAKE_AUTORATE_PROCD_JOB_JSONFILTER="$work/bin/jsonfilter"
export CAKE_AUTORATE_PROCD_JOB_SETSID="$(command -v setsid)"
export CAKE_AUTORATE_PROCD_JOB_TEST_COMMAND_ROOT="$work/bin"

result="$($helper launch cake-autorate-test worker \
	/tmp/cake-autorate-speedtest/procd-job-test.log "$work/bin/test-worker" hello)"
worker_pid="$(printf '%s\n' "$result" | sed -n 's/.*"pid":\([0-9][0-9]*\).*/\1/p')"
worker_start="$(printf '%s\n' "$result" | sed -n 's/.*"starttime":\([0-9][0-9]*\).*/\1/p')"
[ -n "$worker_pid" ] && [ -n "$worker_start" ]
stat_tail="$(sed 's/^.*) //' "/proc/$worker_pid/stat")"
[ "$(printf '%s\n' "$stat_tail" | awk '{ print $3 }')" = "$worker_pid" ]
[ "$(printf '%s\n' "$stat_tail" | awk '{ print $4 }')" = "$worker_pid" ]
[ "$(printf '%s\n' "$stat_tail" | awk '{ print $20 }')" = "$worker_start" ]

if "$helper" launch cake-autorate-test worker /tmp/not-allowed.log \
	"$work/bin/test-worker" >/dev/null 2>&1; then
	echo "unsafe procd log path was accepted" >&2
	exit 1
fi
if "$helper" launch cake-autorate-test worker \
	/tmp/cake-autorate-quality/procd-job-test/job.log \
	"$work/bin/test-worker" >/dev/null 2>&1; then
	echo "retired quality-test log path was accepted" >&2
	exit 1
fi
if "$helper" launch cake-autorate-test worker \
	/tmp/cake-autorate-speedtest/procd-job-test.log /bin/sh >/dev/null 2>&1; then
	echo "unallowlisted procd worker command was accepted" >&2
	exit 1
fi

printf '%s\n' 'procd job launcher tests passed'
