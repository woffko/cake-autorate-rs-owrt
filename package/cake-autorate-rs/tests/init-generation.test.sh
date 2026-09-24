#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/cake-init-generation-test.XXXXXX")"
trap 'rm -rf "$work"' EXIT INT TERM
id=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
for variant in full lite; do
	sh "$base/scripts/render-init-variant.sh" "$variant" \
		"$base/files/etc/init.d/cake-autorate" "$work/$variant"
	(
		. "$work/$variant"
		events="$work/events"
		: > "$events"
		logger() { :; }
		service_prepare_start() { printf '%s\n' "$plan"; }
		procd_open_instance() { printf 'open:%s\n' "$1" >> "$events"; }
		procd_set_param() { printf 'set:%s\n' "$*" >> "$events"; }
		procd_close_instance() { printf '%s\n' close >> "$events"; }
		plan="service-start-v3 lab:$id,backup:$id -"
		start_service_locked || exit 1
		[ "$(grep -c '^open:' "$events")" = 2 ] || exit 1
		[ "$(grep -c "^set:env CAKE_AUTORATE_SERVICE_CONFIG_ID=$id$" "$events")" = 2 ] || exit 1
		grep -Fxq 'set:command /usr/sbin/cake-autorated --instance lab' "$events" || exit 1
		grep -Fxq 'set:command /usr/sbin/cake-autorated --instance backup' "$events" || exit 1
		: > "$events"
		plan='service-start-v3 - -'
		start_service_locked || exit 1
		[ ! -s "$events" ] || exit 1
		for plan in \
			'service-start-v3' \
			'service-start-v3 -' \
			'service-start-v3 - - extra' \
			"service-start-v3 lab:$id,bad:short -" \
			"service-start-v3 lab:$id,lab:$id -" \
			"service-start-v3 lab:$id:extra -" \
			"service-start-v3 lab:${id%?} -" \
			"service-start-v3 lab:F${id#?} -" \
			"service-start-v3 bad-name:$id -" \
			"service-start-v3 :$id -" \
			"service-start-v3 lab:$id, -"; do
			if start_service_locked; then
				echo "malformed generation response accepted ($variant)" >&2
				exit 1
			fi
			[ ! -s "$events" ] || { echo 'partial generation response reached procd' >&2; exit 1; }
		done
		pairs=""
		i=0
		while [ "$i" -lt 65 ]; do
			pairs="${pairs:+$pairs,}lab$i:$id"
			i=$((i + 1))
		done
		plan="service-start-v3 $pairs -"
		if start_service_locked; then exit 1; fi
		[ ! -s "$events" ] || exit 1
	) || { echo "$variant generation protocol test failed" >&2; exit 1; }
	# Real rc.common calls close_service even when start_service returns an
	# error. The callback must exit, not return into this unconditional close.
	for failure in preparation protocol; do
		if (
			. "$work/$variant"
			logger() { :; }
			service_runtime_lock_acquire_or_exit() { :; }
			service_runtime_lock_release_or_exit() { :; }
			service_prepare_start() {
				[ "$failure" != preparation ] || return 1
				printf '%s\n' invalid-protocol
			}
			start_service
			: > "$work/serialized"
		); then
			echo "$variant serialized procd after $failure failure" >&2
			exit 1
		fi
		[ ! -e "$work/serialized" ] || exit 1
	done
	# rc.common must never serialize incomplete JSON after any failed procd
	# parameter, including failure while attaching the generation environment.
	for fail in command env respawn stdout stderr; do
		if (
			. "$work/$variant"
			logger() { :; }
			service_prepare_start() { printf '%s\n' "service-start-v3 lab:$id -"; }
			procd_open_instance() { :; }
			procd_set_param() { [ "$1" != "$fail" ]; }
			procd_close_instance() { :; }
			start_service_locked
			: > "$work/serialized"
		); then
			echo "$variant ignored procd $fail failure" >&2
			exit 1
		fi
		[ ! -e "$work/serialized" ] || exit 1
	done
done

echo 'init Full/Lite generation protocol and partial procd rejection tests passed'
