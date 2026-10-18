#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/cake-init-boot-retry-test.XXXXXX")"
trap 'rm -rf "$work"' EXIT INT TERM
export WORK="$work"

# A stand-in init script: every invocation records its action and the first
# $FAILURES starts fail.
cat > "$work/initscript" <<'EOF'
#!/bin/sh
printf '%s\n' "$1" >> "$WORK/attempts"
[ "$(wc -l < "$WORK/attempts")" -gt "$FAILURES" ]
EOF
# The native policy without its pause: three attempts in total.
cat > "$work/daemon" <<'EOF'
#!/bin/sh
[ "$1 $2" = '--service-lifecycle boot-retry' ] || exit 64
printf '%s\n' "$3" >> "$WORK/retries"
if [ "$3" -ge 3 ]; then
	echo 'ERROR: start failed at boot after 3 attempts' >&2
	exit 1
fi
echo "WARNING: start failed at boot, retrying ($(($3 + 1))/3)" >&2
echo "service-boot-retry-v1 $(($3 + 1))"
EOF
chmod 0755 "$work/initscript" "$work/daemon"

for variant in full lite; do
	sh "$base/scripts/render-init-variant.sh" "$variant" \
		"$base/files/etc/init.d/cake-autorate" "$work/$variant"
	for case in 0 1 2 3 disabled; do
		: > "$work/attempts"
		: > "$work/retries"
		: > "$work/log"
		code=0
		(
			. "$work/$variant"
			initscript="$work/initscript"
			DAEMON="$work/daemon"
			logger() { cat >> "$WORK/log"; }
			if [ "$case" = disabled ]; then
				export FAILURES=3
				enabled() { return 1; }
			else
				export FAILURES="$case"
				enabled() { return 0; }
			fi
			boot || code=$?
			wait
			exit "$code"
		) || code=$?
		attempts="$(wc -l < "$work/attempts")"
		retries="$(tr '\n' ' ' < "$work/retries")"
		case "$case" in
			0) want_code=0 want_attempts=1 want_retries='' ;;
			1) want_code=1 want_attempts=2 want_retries='1 ' ;;
			2) want_code=1 want_attempts=3 want_retries='1 2 ' ;;
			3) want_code=1 want_attempts=3 want_retries='1 2 3 ' ;;
			disabled) want_code=1 want_attempts=1 want_retries='1 ' ;;
		esac
		[ "$code" = "$want_code" ] && [ "$attempts" = "$want_attempts" ] &&
			[ "$retries" = "$want_retries" ] || {
			echo "$variant boot $case: code $code, $attempts attempts, retries '$retries'" >&2
			exit 1
		}
		[ "$(sort -u "$work/attempts")" = start ] || {
			echo "$variant boot $case used another action than start" >&2
			exit 1
		}
		# The decisions reach syslog; the protocol line does not.
		! grep -q service-boot-retry "$work/log" || exit 1
		if [ "$case" = 3 ]; then
			grep -Fq 'ERROR: start failed at boot after 3 attempts' "$work/log" || exit 1
		fi
		if [ "$want_retries" != '' ] && [ "$case" != disabled ]; then
			grep -Fq 'retrying (2/3)' "$work/log" || exit 1
		fi
	done
done

# rc.common takes procd_lock on descriptor 1000 for every init action, and the
# children of the boot script inherit it. BusyBox ash (as on the router) can
# hold that descriptor: the stand-in below takes the lock the way procd_lock
# does, and a retry must not wait for a lock still held by the retry loop or
# by its logger once the boot script itself has exited.
if command -v busybox >/dev/null 2>&1; then
	cat > "$work/locking-initscript" <<'EOF'
#!/bin/sh
exec busybox sh -c '
	flock -n 1000 >/dev/null 2>&1 || {
		exec 1000>"$WORK/procd.lock"
		flock -w 5 1000 || { echo blocked >> "$WORK/blocked"; exit 1; }
	}
	printf "%s\n" "$0" >> "$WORK/attempts"
	[ "$(wc -l < "$WORK/attempts")" -gt "$FAILURES" ]
' "$1"
EOF
	chmod 0755 "$work/locking-initscript"
	for variant in full lite; do
		: > "$work/attempts"
		: > "$work/retries"
		: > "$work/blocked"
		: > "$work/log"
		FAILURES=1 busybox sh -c '
			exec 1000>"$WORK/procd.lock"
			flock 1000
			. "$1"
			initscript="$2"
			DAEMON="$3"
			logger() { cat >> "$WORK/log"; }
			enabled() { return 0; }
			boot || :
		' sh "$work/$variant" "$work/locking-initscript" "$work/daemon"
		i=0
		while [ "$(wc -l < "$work/attempts")" -lt 2 ] && [ ! -s "$work/blocked" ] &&
			[ "$i" -lt 150 ]; do
			sleep 0.1
			i=$((i + 1))
		done
		[ ! -s "$work/blocked" ] && [ "$(wc -l < "$work/attempts")" = 2 ] || {
			echo "$variant boot retry waited for an inherited procd_lock" >&2
			exit 1
		}
	done
else
	echo 'busybox is not installed: the inherited procd_lock case was not run'
fi

# The pause and the bound belong to the native lifecycle, not to the shell.
! grep -Eq '(^|[^_])(sleep|usleep)([^_]|$)' "$base/files/etc/init.d/cake-autorate"
grep -Fq '"$DAEMON" --service-lifecycle boot-retry "$attempt"' \
	"$base/files/etc/init.d/cake-autorate"

echo 'init Full/Lite bounded boot start retry tests passed'
