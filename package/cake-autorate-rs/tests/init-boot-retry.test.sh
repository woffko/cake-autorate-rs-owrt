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

# The pause and the bound belong to the native lifecycle, not to the shell.
! grep -Eq '(^|[^_])(sleep|usleep)([^_]|$)' "$base/files/etc/init.d/cake-autorate"
grep -Fq '"$DAEMON" --service-lifecycle boot-retry "$attempt"' \
	"$base/files/etc/init.d/cake-autorate"

echo 'init Full/Lite bounded boot start retry tests passed'
