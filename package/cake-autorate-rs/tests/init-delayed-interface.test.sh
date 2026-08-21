#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/cake-init-delayed-interface-test.XXXXXX")"
daemon="$work/cake-autorated"
trap 'rm -rf "$work"' EXIT INT TERM

cat >"$daemon" <<'EOF'
#!/bin/sh
if [ "${CAKE_TEST_LITE:-0}" = 1 ]; then
	printf '%s\n' 'service-start-v1 delayed'
else
	printf '%s\n' 'service-start-v2 delayed -'
fi
EOF
chmod +x "$daemon"

for variant in full lite; do
	init_script="$work/cake-autorate.$variant"
	events="$work/events.$variant"
	sh "$base/scripts/render-init-variant.sh" "$variant" \
		"$base/files/etc/init.d/cake-autorate" "$init_script"

	grep -Fq '"$DAEMON" --service-lifecycle prepare-start' "$init_script"
	if grep -Eq '^start_instance\(\)|^instance_has_(startable_config|enabled_sqm_backing)\(\)|^section_has_sqm_conflict\(\)' "$init_script"; then
		echo "$variant init still owns delayed-interface admission decisions" >&2
		exit 1
	fi

	(
		. "$init_script"
		DAEMON="$daemon"
		[ "$variant" != lite ] || CAKE_TEST_LITE=1
		export CAKE_TEST_LITE
		logger() { :; }
		procd_open_instance() { printf 'open:%s\n' "$1" >> "$events"; }
		procd_set_param() { :; }
		procd_close_instance() { printf '%s\n' close >> "$events"; }
		start_service_locked
	)

	[ "$(cat "$events")" = 'open:delayed
close' ] || {
		echo "$variant init second-guessed the native delayed-interface plan" >&2
		cat "$events" >&2
		exit 1
	}
done

echo "init delayed-interface bridge tests passed"
