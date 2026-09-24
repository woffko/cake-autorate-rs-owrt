#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/cake-init-preflight-test.XXXXXX")"
trap 'rm -rf "$work"' EXIT INT TERM
source_before=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
source_after=ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff

for variant in full lite; do
	sh "$base/scripts/render-init-variant.sh" "$variant" \
		"$base/files/etc/init.d/cake-autorate" "$work/$variant"
	sh -n "$work/$variant"
	for operation in reload_service restart; do
		for response in failed empty malformed legacy extra valid unchanged; do
			(
				. "$work/$variant"
				requested_operation="$operation"
				log="$work/events"
				: > "$log"
				DAEMON=fake_daemon
				logger() { :; }
				service_report_error() { :; }
				# Actual inherited fd semantics are checked by init-runtime-lock.
				service_runtime_lock_acquire_or_exit() { :; }
				service_runtime_lock_release_or_exit() { :; }
				fake_daemon() {
					# This matrix covers the explicit legacy migration branch.
					if [ "$*" = '--service-lifecycle reload' ]; then
						[ "$requested_operation" = reload_service ] || return 96
						printf '%s\n' 'service-reload-v1 legacy'
						return 0
					fi
					case "$*" in '--service-lifecycle preflight-start'|'--service-lifecycle preflight-reload') ;; *) return 90 ;; esac
					if [ "$requested_operation" = restart ]; then
						[ "$*" = '--service-lifecycle preflight-start' ] || return 91
					else
						[ "$*" = '--service-lifecycle preflight-reload' ] || return 91
					fi
					printf '%s\n' preflight >> "$log"
					case "$response" in
						failed) return 72 ;;
						empty) : ;;
						malformed) printf '%s\n' 'service-preflight-v2 ok' ;;
						legacy) printf '%s\n' 'service-preflight-v1 ok' ;;
						extra) printf '%s\n' "service-preflight-v2 $source_before extra" ;;
						valid) printf '%s\n' "service-preflight-v2 $source_before" ;;
						unchanged) printf '%s\n' "service-preflight-unchanged-v1 $source_before" ;;
						esac
				}
				stop() {
					[ "$SERVICE_SOURCE_ID" = "$source_before" ] || return 91
					printf '%s\n' stop >> "$log"
					service_accept_stop_result "service-stop-v2 $source_after"
				}
				start() {
					[ "$SERVICE_SOURCE_ID" = "$source_after" ] || return 92
					printf '%s\n' start >> "$log"
				}
				code=0
				"$operation" || code=$?
				if [ "$response" = valid ]; then
					[ "$code" = 0 ] || exit 1
					[ "$(cat "$log")" = "$(printf 'preflight\nstop\nstart')" ] || exit 1
				elif [ "$response" = unchanged ] && [ "$operation" = reload_service ]; then
					[ "$code" = 0 ] || exit 1
					[ "$(cat "$log")" = preflight ] || exit 1
				else
					[ "$code" != 0 ] || exit 1
					[ "$(cat "$log")" = preflight ] || exit 1
				fi
			) || {
				echo "$variant $operation violated preflight ordering ($response)" >&2
				exit 1
			}
		done
	done
	(
		. "$work/$variant"
		logger() { :; }
		service_runtime_lock_acquire_or_exit() { :; }
		service_runtime_lock_release_or_exit() { :; }
		DAEMON=fake_source_daemon
		fake_source_daemon() {
			case "$*" in
				'--service-lifecycle preflight-start'|'--service-lifecycle preflight-reload') printf '%s\n' "service-preflight-v2 $source_before" ;;
				'--service-lifecycle execute-stop')
					[ "$CAKE_AUTORATE_SERVICE_SOURCE_ID" = "$source_before" ] || return 93
					printf '%s\n' "service-stop-v2 $source_after" ;;
				'--service-lifecycle prepare-start')
					[ "$CAKE_AUTORATE_SERVICE_SOURCE_ID" = "$source_after" ] || return 94
					printf '%s\n' 'service-start-v3 - -' ;;
				*) return 95 ;;
			esac
		}
		service_preflight_start
		stop_service
		[ "$(service_prepare_start)" = 'service-start-v3 - -' ]
		for invalid in 'service-stop-v1 ok' 'service-stop-v2 short' "service-stop-v2 $source_after extra"; do
			if service_accept_stop_result "$invalid"; then exit 1; fi
			[ "$SERVICE_SOURCE_ID" = "$source_after" ] || exit 1
		done
		SERVICE_SOURCE_ID=
		service_accept_stop_result 'service-stop-v1 ok'
	) || { echo "$variant lost its source-bound lifecycle handoff" >&2; exit 1; }
	if (
		. "$work/$variant"
		SERVICE_SOURCE_ID="$source_before"
		service_runtime_lock_acquire_or_exit() { :; }
		service_report_error() { :; }
		DAEMON=source_changed
		source_changed() {
			[ "$CAKE_AUTORATE_SERVICE_SOURCE_ID" = "$source_before" ] || exit 95
			return 76
		}
		stop_service
		: > "$work/procd-killed"
	); then
		echo "$variant accepted a stale source at Stop" >&2
		exit 1
	fi
	[ ! -e "$work/procd-killed" ] || exit 1
done

echo "init Full/Lite source-bound preflight, restored-source handoff and healthy-service retention tests passed"
