#!/bin/sh
set -eu
base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/cake-selective-reload.XXXXXX")"
trap 'rm -rf "$work"' EXIT INT TERM
for variant in full lite; do
	sh "$base/scripts/render-init-variant.sh" "$variant" "$base/files/etc/init.d/cake-autorate" "$work/$variant"
	for response in ready unchanged failed empty malformed extra legacy; do
		(
			. "$work/$variant"
			log="$work/events"
			: > "$log"
			DAEMON=fake_daemon
			logger() { :; }
			service_report_error() { :; }
			fake_daemon() {
				[ "$*" = '--service-lifecycle reload' ] || return 90
				printf '%s\n' native >> "$log"
				case "$response" in
					failed) return 71 ;;
					empty) : ;;
					malformed) printf '%s\n' 'unrecognized' ;;
					extra) printf '%s\n' 'service-reload-v1 ready extra' ;;
					*) printf '%s\n' "service-reload-v1 $response" ;;
				esac
			}
			service_runtime_lock_acquire_or_exit() { :; }
			service_runtime_lock_release_or_exit() { :; }
			service_preflight_start() { printf '%s\n' preflight >> "$log"; }
			stop() { printf '%s\n' stop >> "$log"; }
			start() { printf '%s\n' start >> "$log"; }
			code=0
			reload_service || code=$?
			case "$response" in
				ready|unchanged) [ "$code" = 0 ]; [ "$(cat "$log")" = native ] ;;
				legacy) [ "$code" = 0 ]; [ "$(cat "$log")" = "$(printf 'native\npreflight\nstop\nstart')" ] ;;
				*) [ "$code" != 0 ]; [ "$(cat "$log")" = native ] ;;
			esac
		) || { echo "$variant selective reload protocol failed: $response" >&2; exit 1; }
	done
done
echo 'Full/Lite selective reload routing and fail-closed legacy fallback passed'
