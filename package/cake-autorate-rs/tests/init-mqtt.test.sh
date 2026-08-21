#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/cake-mqtt-init-test.XXXXXX")"
trap 'rm -rf "$work"' EXIT INT TERM
rendered="$work/cake-autorate.full"
daemon="$work/cake-autorated"
events="$work/events"

sh "$base/scripts/render-init-variant.sh" full \
	"$base/files/etc/init.d/cake-autorate" "$rendered"

cat >"$daemon" <<'EOF'
#!/bin/sh
[ "$*" = '--service-lifecycle prepare-start' ] || exit 2
printf '%s\n' "${MQTT_TEST_PLAN:-service-start-v2 wan_a,wan_b wan_a,wan_b}"
EOF
chmod +x "$daemon"

logger() { :; }
procd_open_instance() { printf 'open:%s\n' "$1" >>"$events"; }
procd_set_param() { printf 'set:%s\n' "$*" >>"$events"; }
procd_close_instance() { printf 'close\n' >>"$events"; }

. "$rendered"
DAEMON="$daemon"
start_service_locked

expected="open:wan_a
set:command $daemon --instance wan_a
set:respawn 3600 5 5
set:stdout 1
set:stderr 1
close
open:wan_b
set:command $daemon --instance wan_b
set:respawn 3600 5 5
set:stdout 1
set:stderr 1
close
open:mqtt_wan_a
set:command $daemon --mqtt-publisher wan_a
set:respawn 3600 5 5
set:term_timeout 5
set:stdout 1
set:stderr 1
close
open:mqtt_wan_b
set:command $daemon --mqtt-publisher wan_b
set:respawn 3600 5 5
set:term_timeout 5
set:stdout 1
set:stderr 1
close"
[ "$(cat "$events")" = "$expected" ] || {
	echo 'main init did not construct the exact controller plus MQTT instance set' >&2
	exit 1
}

# Both lists are validated before the first procd call. A malformed MQTT list
# therefore cannot publish an otherwise-valid controller prefix.
for malformed in \
	'service-start-v2 wan mqtt,mqtt' \
	'service-start-v2 wan bad-name' \
	'service-start-v2 wan ,mqtt' \
	'service-start-v2 wan mqtt,'; do
	: >"$events"
	MQTT_TEST_PLAN="$malformed"
	export MQTT_TEST_PLAN
	if start_service_locked >/dev/null 2>&1; then
		echo "main init accepted malformed MQTT plan: $malformed" >&2
		exit 1
	fi
	[ ! -s "$events" ] || {
		echo "malformed MQTT plan published a partial procd service: $malformed" >&2
		exit 1
	}
done

if grep -Eq 'cake-autorate-mqtt|mqtt-service-plan|mosquitto|sleep|usleep' "$rendered"; then
	echo 'main init still delegates MQTT selection, transport, retry, or lifecycle policy' >&2
	exit 1
fi
grep -Fq 'procd_set_param command "$DAEMON" --mqtt-publisher "$1"' "$rendered"

printf '%s\n' 'native MQTT unified procd lifecycle tests passed'
