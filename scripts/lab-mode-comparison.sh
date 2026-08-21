#!/bin/sh

# RAM-only CAKE mode comparison for an explicitly designated test router.
# It never writes UCI. One invocation measures one mode, then restores the
# managed SQM runtime before releasing the per-interface maintenance lock.

set -u

mode="${1:-}"
case "$mode" in
	shaped|custom_shaped|off|ul_shaped|dl_shaped|unlimited|ul_unlimited|dl_unlimited) ;;
	*)
		echo "usage: $0 shaped|custom_shaped|off|ul_shaped|dl_shaped|unlimited|ul_unlimited|dl_unlimited" >&2
		exit 2
		;;
esac

section="${CAKE_LAB_SECTION:-wwan_adaptive}"
wan="${CAKE_LAB_WAN:-wwan0}"
managed_ifb="${CAKE_LAB_MANAGED_IFB:-ifb4wwan0}"
lab_ifb="${CAKE_LAB_IFB:-ifbLabWwan0}"
server_id="${CAKE_LAB_SERVER_ID:-}"
reflectors="${CAKE_LAB_REFLECTORS:-1.1.1.1 8.8.8.8 9.9.9.9}"
transport_endpoint="${CAKE_LAB_TRANSPORT_ENDPOINT:-wss://ping-bufferbloat.libreqos.com/ws}"
runtime_lock_lib="${CAKE_AUTORATE_RUNTIME_LOCK_LIB:-/usr/libexec/cake-autorate-rs/runtime-lock}"
sqm_run="${CAKE_LAB_SQM_RUN:-/usr/lib/sqm/run.sh}"
daemon="${CAKE_LAB_DAEMON:-/usr/sbin/cake-autorated}"
speedtest="${CAKE_LAB_SPEEDTEST:-/usr/bin/speedtest-go}"
fping_bin="${CAKE_LAB_FPING:-/usr/bin/fping}"
nft_bin="${CAKE_LAB_NFT:-/usr/sbin/nft}"
jsonfilter_bin="${CAKE_LAB_JSONFILTER:-/usr/bin/jsonfilter}"

stamp="$(date +%Y%m%d-%H%M%S)"
out="${CAKE_LAB_OUTPUT:-/tmp/cake-mode-lab/$stamp-$mode}"
nft_table="cake_lab_$$"
log_file="$out/lab.log"
summary_file="$out/summary.tsv"
marker="$out/cpu.marker"

mkdir -p "$out" || exit 1
chmod 700 "$out" 2>/dev/null || true

log() {
	printf '%s %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" | tee -a "$log_file"
}

die() {
	log "ERROR: $*"
	exit 1
}

is_uint() {
	case "$1" in ''|*[!0-9]*) return 1 ;; esac
}

proc_starttime() {
	pid="$1"
	[ -r "/proc/$pid/stat" ] || return 1
	sed 's/^.*) //' "/proc/$pid/stat" 2>/dev/null | awk 'NF >= 20 { print $20; exit }'
}

proc_state() {
	pid="$1"
	[ -r "/proc/$pid/stat" ] || return 1
	sed 's/^.*) //' "/proc/$pid/stat" 2>/dev/null | awk 'NF >= 1 { print $1; exit }'
}

wait_for_state() {
	pid="$1"
	wanted="$2"
	attempt=0
	while [ "$attempt" -lt 10 ]; do
		[ "$(proc_state "$pid" 2>/dev/null)" = "$wanted" ] && return 0
		attempt=$((attempt + 1))
		sleep 1
	done
	return 1
}

config_fingerprint() {
	sha256sum /etc/config/cake-autorate /etc/config/sqm 2>/dev/null | sort
}

managed_runtime_present() {
	tc qdisc show dev "$wan" 2>/dev/null | awk '$2 == "cake" && $4 == "root" { found=1 } END { exit(found ? 0 : 1) }' &&
	tc qdisc show dev "$wan" 2>/dev/null | grep -Eq ' ingress | clsact ' &&
	[ -e "/sys/class/net/$managed_ifb" ] &&
	tc qdisc show dev "$managed_ifb" 2>/dev/null | awk '$2 == "cake" && $4 == "root" { found=1 } END { exit(found ? 0 : 1) }'
}

lab_runtime_absent() {
	[ ! -e "/sys/class/net/$lab_ifb" ] &&
	! tc qdisc show dev "$wan" 2>/dev/null | grep -q 'handle 7a01:' &&
	! tc qdisc show dev "$wan" 2>/dev/null | grep -q 'handle 7aff:'
}

cleanup_done=0
snapshot_ready=0
autorate_paused=0
autorate_pid=""
autorate_start=""
sqm_was_active=0
sqm_stopped=0
lab_root_owned=0
lab_ingress_owned=0
lab_ifb_owned=0
nft_owned=0
locks_held=0
cleanup_ok=1
ping_pid=""
transport_idle_pid=""
loaded_ping_pid=""
loaded_transport_pid=""
cpu_pid=""
speed_dl_pid=""
speed_ul_pid=""

stop_owned_pid() {
	owned_pid="$1"
	[ -n "$owned_pid" ] || return 0
	kill -TERM "$owned_pid" 2>/dev/null || true
	wait "$owned_pid" 2>/dev/null || true
}

restore_runtime() {
	[ "$cleanup_done" -eq 0 ] || return 0
	cleanup_done=1
	set +e
	rm -f "$marker"
	stop_owned_pid "$ping_pid"
	stop_owned_pid "$transport_idle_pid"
	stop_owned_pid "$loaded_ping_pid"
	stop_owned_pid "$loaded_transport_pid"
	stop_owned_pid "$cpu_pid"
	stop_owned_pid "$speed_dl_pid"
	stop_owned_pid "$speed_ul_pid"
	ping_pid=""
	transport_idle_pid=""
	loaded_ping_pid=""
	loaded_transport_pid=""
	cpu_pid=""
	speed_dl_pid=""
	speed_ul_pid=""
	[ "$nft_owned" -eq 0 ] || "$nft_bin" delete table inet "$nft_table" >/dev/null 2>&1
	nft_owned=0

	if [ "$lab_ingress_owned" -eq 1 ]; then
		tc qdisc del dev "$wan" ingress >/dev/null 2>&1 || cleanup_ok=0
	fi
	if [ "$lab_root_owned" -eq 1 ]; then
		tc qdisc del dev "$wan" root >/dev/null 2>&1 || cleanup_ok=0
	fi
	if [ "$lab_ifb_owned" -eq 1 ]; then
		tc qdisc del dev "$lab_ifb" root >/dev/null 2>&1 || true
		ip link set dev "$lab_ifb" down >/dev/null 2>&1 || true
		ip link delete dev "$lab_ifb" type ifb >/dev/null 2>&1 || cleanup_ok=0
	fi
	lab_ingress_owned=0
	lab_root_owned=0
	lab_ifb_owned=0

	if [ "$sqm_was_active" -eq 1 ] && [ "$sqm_stopped" -eq 1 ]; then
		"$sqm_run" start "$wan" >> "$log_file" 2>&1 || cleanup_ok=0
		restore_wait=0
		while [ "$restore_wait" -lt 10 ] && ! managed_runtime_present; do
			restore_wait=$((restore_wait + 1))
			sleep 1
		done
		managed_runtime_present || cleanup_ok=0
		sqm_stopped=0
	fi

	if [ "$autorate_paused" -eq 1 ] && [ -n "$autorate_pid" ]; then
		if [ "$(proc_starttime "$autorate_pid" 2>/dev/null)" = "$autorate_start" ]; then
			kill -CONT "$autorate_pid" >/dev/null 2>&1 || cleanup_ok=0
		else
			cleanup_ok=0
		fi
	fi
	autorate_paused=0

	if [ "$locks_held" -eq 1 ]; then
		runtime_lock_release_interface >/dev/null 2>&1 || cleanup_ok=0
		runtime_lock_release_global >/dev/null 2>&1 || cleanup_ok=0
		locks_held=0
	fi

	config_fingerprint > "$out/config.after.sha256"
	tc qdisc show > "$out/qdisc.after.txt" 2>&1
	ip -d link show > "$out/link.after.txt" 2>&1
	uci changes > "$out/uci-changes.after.txt" 2>&1
	lab_runtime_absent || cleanup_ok=0
	if [ "$snapshot_ready" -eq 1 ] && ! cmp -s "$out/config.before.sha256" "$out/config.after.sha256"; then
		cleanup_ok=0
	fi
	if [ -s "$out/uci-changes.after.txt" ]; then
		cleanup_ok=0
	fi
	if [ "$cleanup_ok" -eq 1 ]; then
		log "RESTORE_OK: managed runtime and UCI fingerprints verified"
	else
		log "RESTORE_FAILED: inspect $out before any further experiment"
	fi
}

on_exit() {
	rc="$?"
	trap - EXIT HUP INT TERM
	restore_runtime
	[ "$cleanup_ok" -eq 1 ] || rc=1
	exit "$rc"
}

trap on_exit EXIT
trap 'exit 130' HUP INT TERM

[ -r "$runtime_lock_lib" ] || die "runtime lock library is missing"
# shellcheck disable=SC1090
. "$runtime_lock_lib"

for required in "$sqm_run" "$daemon" "$speedtest" "$fping_bin" "$nft_bin" "$jsonfilter_bin" tc ip awk sed sort sha256sum; do
	command -v "$required" >/dev/null 2>&1 || die "required command is missing: $required"
done

[ "$(id -u)" = 0 ] || die "must run as root"
[ -e "/sys/class/net/$wan" ] || die "target interface $wan is absent"
[ "$(uci -q get "cake-autorate.$section.wan_if" 2>/dev/null)" = "$wan" ] ||
	die "instance $section does not own $wan"
[ "$(uci -q get "cake-autorate.$section.manage_sqm" 2>/dev/null)" = 1 ] ||
	die "instance $section does not manage SQM"
[ "$(uci -q get "cake-autorate.$section.sqm_enabled" 2>/dev/null)" = 1 ] ||
	die "managed SQM is disabled"
[ -z "$(uci changes)" ] || die "UCI has unsaved changes"
managed_runtime_present || die "managed SQM runtime is not healthy before the experiment"
[ ! -e "/sys/class/net/$lab_ifb" ] || die "temporary IFB $lab_ifb already exists"

source_ip="$(ip -4 addr show dev "$wan" 2>/dev/null | awk '/inet / { sub(/\/.*/, "", $2); print $2; exit }')"
[ -n "$source_ip" ] || die "no IPv4 source address on $wan"
[ "$(ip -4 route show default 2>/dev/null | awk 'NR == 1 { for (i=1; i<=NF; i++) if ($i == "dev") print $(i+1) }')" = "$wan" ] ||
	die "$wan is not the main default route"

if [ -z "$server_id" ]; then
	server_id="$(cat "/tmp/cake-autorate-speedtest/$section.speedtest-go-server" 2>/dev/null || true)"
fi
is_uint "$server_id" || die "no pinned speedtest-go server ID"

dl_rate="$(uci -q get "cake-autorate.$section.sqm_download" 2>/dev/null)"
ul_rate="$(uci -q get "cake-autorate.$section.sqm_upload" 2>/dev/null)"
is_uint "$dl_rate" || die "invalid managed download rate"
is_uint "$ul_rate" || die "invalid managed upload rate"
if [ "$mode" = custom_shaped ]; then
	dl_rate="${CAKE_LAB_DL_RATE:-}"
	ul_rate="${CAKE_LAB_UL_RATE:-}"
	is_uint "$dl_rate" || die "custom_shaped requires CAKE_LAB_DL_RATE"
	is_uint "$ul_rate" || die "custom_shaped requires CAKE_LAB_UL_RATE"
	[ "$dl_rate" -ge 1000 ] || die "custom download rate is implausibly low"
	[ "$ul_rate" -ge 1000 ] || die "custom upload rate is implausibly low"
fi

config_fingerprint > "$out/config.before.sha256"
snapshot_ready=1
tc qdisc show > "$out/qdisc.before.txt" 2>&1
ip -d link show > "$out/link.before.txt" 2>&1
uci export cake-autorate > "$out/cake-autorate.before.uci"
uci export sqm > "$out/sqm.before.uci"
printf 'mode\tdirection\tthroughput_kbps\ticmp_idle_p95_ms\ticmp_loaded_p95_ms\ticmp_delta_ms\ticmp_loss_percent\ttransport_idle_p95_ms\ttransport_loaded_p95_ms\ttransport_delta_ms\teffective_delta_ms\tgrade\tcpu_p95_percent\tsoftirq_p95_percent\tbackground_dl_kbps\tbackground_ul_kbps\tserver_id\n' > "$summary_file"

runtime_lock_acquire_global_shared || die "another global SQM mutation is active"
lock_token="lab${stamp#????????-}$$"
runtime_lock_acquire_interface "$wan" labtest "" "$lock_token" || die "another operation owns $wan"
locks_held=1

autorate_candidates="$(pgrep -f "^$daemon --instance $section\$" 2>/dev/null || true)"
set -- $autorate_candidates
[ "$#" -eq 1 ] || die "expected exactly one $section daemon, found $#"
autorate_pid="$1"
autorate_start="$(proc_starttime "$autorate_pid" 2>/dev/null)"
is_uint "$autorate_start" || die "unable to identify autorate process"
kill -STOP "$autorate_pid" 2>/dev/null || die "unable to pause autorate"
wait_for_state "$autorate_pid" T || die "autorate did not enter stopped state"
autorate_paused=1
sqm_was_active=1

stop_managed_sqm() {
	[ "$sqm_stopped" -eq 0 ] || return 0
	"$sqm_run" stop "$wan" >> "$log_file" 2>&1 || die "unable to stop managed SQM"
	sqm_stopped=1
	post="$(tc qdisc show dev "$wan" 2>/dev/null)"
	printf '%s\n' "$post" | grep -q ' cake ' && die "CAKE remained on $wan after managed stop"
	printf '%s\n' "$post" | grep -Eq ' ingress | clsact ' && die "ingress remained on $wan after managed stop"
}

add_lab_root() {
	kind="$1"
	case "$kind" in
		shaped)
			tc qdisc replace dev "$wan" root handle 7a01: cake bandwidth "${ul_rate}Kbit" besteffort triple-isolate nat nowash split-gso rtt 100ms raw overhead 0 ||
				die "unable to attach shaped upload CAKE"
			;;
		unlimited)
			tc qdisc replace dev "$wan" root handle 7a01: cake unlimited diffserv4 triple-isolate nat nowash split-gso rtt 100ms raw overhead 0 ||
				die "unable to attach unlimited upload CAKE"
			;;
		*) die "invalid lab root kind" ;;
	esac
	lab_root_owned=1
}

add_lab_ingress() {
	kind="$1"
	ip link add name "$lab_ifb" type ifb >/dev/null 2>&1 || die "unable to create $lab_ifb"
	lab_ifb_owned=1
	ip link set dev "$lab_ifb" alias "cake-mode-lab-$stamp" >/dev/null 2>&1 || die "unable to mark $lab_ifb"
	ip link set dev "$lab_ifb" up >/dev/null 2>&1 || die "unable to bring up $lab_ifb"
	case "$kind" in
		shaped)
			tc qdisc replace dev "$lab_ifb" root handle 7a02: cake bandwidth "${dl_rate}Kbit" besteffort triple-isolate nat wash split-gso rtt 100ms raw overhead 0 ||
				die "unable to attach shaped download CAKE"
			;;
		unlimited)
			tc qdisc replace dev "$lab_ifb" root handle 7a02: cake unlimited diffserv4 triple-isolate nat wash split-gso rtt 100ms raw overhead 0 ||
				die "unable to attach unlimited download CAKE"
			;;
		*) die "invalid lab ingress kind" ;;
	esac
	tc qdisc add dev "$wan" handle 7aff: ingress || die "unable to attach lab ingress"
	lab_ingress_owned=1
	# Linux exposes ingress filters below the canonical ffff: parent even when
	# the ingress qdisc itself carries our distinct 7aff: handle.
	tc filter add dev "$wan" parent ffff: protocol all u32 match u32 0 0 action mirred egress redirect dev "$lab_ifb" ||
		die "unable to redirect ingress to $lab_ifb"
}

case "$mode" in
	shaped)
		log "mode shaped: using paused managed CAKE at DL=$dl_rate UL=$ul_rate kbit/s"
		;;
	custom_shaped)
		stop_managed_sqm
		add_lab_root shaped
		add_lab_ingress shaped
		log "mode custom_shaped: temporary CAKE at DL=$dl_rate UL=$ul_rate kbit/s"
		;;
	off)
		stop_managed_sqm
		log "mode off: managed upload and download CAKE bypassed"
		;;
	ul_shaped)
		stop_managed_sqm
		add_lab_root shaped
		log "mode ul_shaped: upload shaped at $ul_rate kbit/s; download bypassed"
		;;
	dl_shaped)
		stop_managed_sqm
		add_lab_ingress shaped
		log "mode dl_shaped: download shaped at $dl_rate kbit/s; upload bypassed"
		;;
	unlimited)
		stop_managed_sqm
		add_lab_root unlimited
		add_lab_ingress unlimited
		log "mode unlimited: CAKE diffserv4 scheduler-only in both directions"
		;;
	ul_unlimited)
		stop_managed_sqm
		add_lab_root unlimited
		log "mode ul_unlimited: upload scheduler-only; download bypassed"
		;;
	dl_unlimited)
		stop_managed_sqm
		add_lab_ingress unlimited
		log "mode dl_unlimited: download scheduler-only; upload bypassed"
		;;
esac

tc qdisc show > "$out/qdisc.mode.txt" 2>&1
ip -d link show > "$out/link.mode.txt" 2>&1

if [ "${CAKE_LAB_DRY_RUN:-0}" = 1 ]; then
	log "dry run: mode switch succeeded; restoring without traffic"
	restore_runtime
	[ "$cleanup_ok" -eq 1 ] || die "dry-run restoration failed"
	log "DRY_RUN_OK: mode $mode switched and restored"
	exit 0
fi

reset_background_counters() {
	if [ "$nft_owned" -eq 1 ]; then
		"$nft_bin" delete table inet "$nft_table" >/dev/null 2>&1 || die "unable to reset background counter table"
		nft_owned=0
	fi
	"$nft_bin" add table inet "$nft_table" || die "unable to add background counter table"
	nft_owned=1
	"$nft_bin" add chain inet "$nft_table" forward '{ type filter hook forward priority -5; policy accept; }' ||
		die "unable to add background counter chain"
	"$nft_bin" add rule inet "$nft_table" forward iifname "$wan" counter comment lab_dl ||
		die "unable to add download background counter"
	"$nft_bin" add rule inet "$nft_table" forward oifname "$wan" counter comment lab_ul ||
		die "unable to add upload background counter"
}

background_bytes() {
	direction="$1"
	"$nft_bin" list chain inet "$nft_table" forward 2>/dev/null | awk -v needle="lab_$direction" '
		index($0, needle) {
			for (i=1; i<=NF; i++) if ($i == "bytes") { print $(i+1); exit }
		}
	'
}

numeric_stats() {
	input="$1"
	sorted="$input.sorted"
	sort -n "$input" > "$sorted"
	awk '
		{ v[NR]=$1 }
		END {
			if (NR < 1) exit 1
			if (NR % 2) med=v[(NR+1)/2]; else med=(v[NR/2]+v[NR/2+1])/2
			p=int((NR*95+99)/100); if (p < 1) p=1
			printf "%.3f %.3f %.3f %d", med, v[p], v[NR], NR
		}
	' "$sorted"
}

extract_icmp_values() {
	input="$1"
	output="$2"
	awk '
		/ bytes,/ {
			for (i=1; i<NF; i++) if ($(i+1) == "ms" && $i ~ /^[0-9]+([.][0-9]+)?$/) print $i
		}
	' "$input" > "$output"
}

icmp_loss_percent() {
	input="$1"
	awk '
		/ : \[[0-9]+\]/ { attempts++ }
		/ bytes,/ { success++ }
		END {
			if (attempts < 1) { print "100.000"; exit }
			printf "%.3f", (attempts-success)*100/attempts
		}
	' "$input"
}

extract_transport_values() {
	input="$1"
	output="$2"
	awk '
		match($0, /"rtt_ms":[0-9]+([.][0-9]+)?/) {
			value=substr($0, RSTART, RLENGTH)
			sub(/^"rtt_ms":/, "", value)
			print value
		}
	' "$input" > "$output"
}

cpu_snapshot() {
	awk '/^cpu / {
		total=0; for (i=2; i<=NF; i++) total+=$i
		idle=$5+$6; soft=$8
		printf "%.0f %.0f %.0f", total, idle, soft
		exit
	}' /proc/stat
}

cpu_monitor() {
	output="$1"
	set -- $(cpu_snapshot)
	prev_total="$1"; prev_idle="$2"; prev_soft="$3"
	while [ -e "$marker" ]; do
		sleep 1
		set -- $(cpu_snapshot)
		total="$1"; idle="$2"; soft="$3"
		awk -v t="$total" -v pt="$prev_total" -v i="$idle" -v pi="$prev_idle" -v s="$soft" -v ps="$prev_soft" '
			BEGIN {
				d=t-pt; if (d <= 0) exit
				printf "%.3f %.3f\n", (d-(i-pi))*100/d, (s-ps)*100/d
			}
		' >> "$output"
		prev_total="$total"; prev_idle="$idle"; prev_soft="$soft"
	done
}

transport_probe() {
	count="$1"
	interval="$2"
	"$daemon" --transport-probe --backend websocket --endpoint "$transport_endpoint" \
		--device "$wan" --source-ip "$source_ip" --count "$count" --timeout 5 --interval-ms "$interval"
}

capture_idle() {
	direction="$1"
	prefix="$out/$direction.idle"
	reset_background_counters
	start="$(date +%s)"
	# shellcheck disable=SC2086
	"$fping_bin" -I "$wan" -D -c 15 -p 200 -- $reflectors > "$prefix.icmp.raw" 2>&1 &
	ping_pid="$!"
	transport_probe 6 500 > "$prefix.transport.raw" 2> "$prefix.transport.err" &
	transport_idle_pid="$!"
	wait "$ping_pid" 2>/dev/null || true
	wait "$transport_idle_pid" 2>/dev/null || true
	ping_pid=""
	transport_idle_pid=""
	end="$(date +%s)"
	elapsed=$((end-start)); [ "$elapsed" -gt 0 ] || elapsed=1
	dl_bytes="$(background_bytes dl)"; ul_bytes="$(background_bytes ul)"
	: "${dl_bytes:=0}"; : "${ul_bytes:=0}"
	awk -v d="$dl_bytes" -v u="$ul_bytes" -v e="$elapsed" 'BEGIN { printf "%.3f %.3f\n", d*8/e/1000, u*8/e/1000 }' > "$prefix.background-kbps"
	extract_icmp_values "$prefix.icmp.raw" "$prefix.icmp.values"
	extract_transport_values "$prefix.transport.raw" "$prefix.transport.values"
	numeric_stats "$prefix.icmp.values" > "$prefix.icmp.stats" || die "$direction idle ICMP evidence is empty"
	numeric_stats "$prefix.transport.values" > "$prefix.transport.stats" || die "$direction idle transport evidence is empty"
}

extract_speed_json() {
	input="$1"
	sed -n '/^{"timestamp"/p' "$input" | tail -n 1
}

run_direction() {
	direction="$1"
	prefix="$out/$direction"
	log "capturing $direction idle baseline"
	capture_idle "$direction"
	# POSIX shell function variables are global unless the implementation
	# provides a non-standard local keyword. capture_idle uses its own prefix,
	# so restore the loaded-phase prefix explicitly after it returns.
	prefix="$out/$direction"
	sleep 2
	reset_background_counters
	start="$(date +%s)"
	# Long bounded monitors are terminated immediately after speedtest-go.
	# shellcheck disable=SC2086
	"$fping_bin" -I "$wan" -D -c 300 -p 200 -- $reflectors > "$prefix.icmp.raw" 2>&1 &
	loaded_ping_pid="$!"
	transport_probe 100 500 > "$prefix.transport.raw" 2> "$prefix.transport.err" &
	loaded_transport_pid="$!"
	: > "$marker"
	cpu_monitor "$prefix.cpu.raw" &
	cpu_pid="$!"

	set -- "$speedtest" --json --unix --ping-mode http --server "$server_id" \
		--source "$source_ip" --dns-bind-source
	case "$direction" in
		download) set -- "$@" --no-upload ;;
		upload) set -- "$@" --no-download ;;
		*) die "invalid direction" ;;
	esac
	log "running $direction speedtest-go against server $server_id"
	"$@" > "$prefix.speedtest.raw" 2> "$prefix.speedtest.err"
	speed_rc="$?"
	end="$(date +%s)"
	rm -f "$marker"
	stop_owned_pid "$loaded_ping_pid"
	stop_owned_pid "$loaded_transport_pid"
	wait "$cpu_pid" 2>/dev/null || true
	loaded_ping_pid=""
	loaded_transport_pid=""
	cpu_pid=""
	[ "$speed_rc" -eq 0 ] || die "$direction speedtest-go exited $speed_rc"

	elapsed=$((end-start)); [ "$elapsed" -gt 0 ] || elapsed=1
	dl_bytes="$(background_bytes dl)"; ul_bytes="$(background_bytes ul)"
	: "${dl_bytes:=0}"; : "${ul_bytes:=0}"
	set -- $(awk -v d="$dl_bytes" -v u="$ul_bytes" -v e="$elapsed" 'BEGIN { printf "%.3f %.3f", d*8/e/1000, u*8/e/1000 }')
	background_dl="$1"; background_ul="$2"

	json="$(extract_speed_json "$prefix.speedtest.raw")"
	[ -n "$json" ] || die "$direction speedtest-go returned no JSON"
	result_server="$($jsonfilter_bin -s "$json" -e '@.servers[0].id' 2>/dev/null)"
	[ "$result_server" = "$server_id" ] || die "$direction speedtest server changed"
	case "$direction" in
		download) speed_bps="$($jsonfilter_bin -s "$json" -e '@.servers[0].dl_speed' 2>/dev/null)" ;;
		upload) speed_bps="$($jsonfilter_bin -s "$json" -e '@.servers[0].ul_speed' 2>/dev/null)" ;;
	esac
	throughput="$(awk -v b="$speed_bps" 'BEGIN { if (b+0 <= 0) exit 1; printf "%.0f", b*8/1000 }')" ||
		die "$direction speedtest returned no positive rate"

	extract_icmp_values "$prefix.icmp.raw" "$prefix.icmp.values"
	extract_transport_values "$prefix.transport.raw" "$prefix.transport.values"
	numeric_stats "$prefix.icmp.values" > "$prefix.icmp.stats" || die "$direction loaded ICMP evidence is empty"
	numeric_stats "$prefix.transport.values" > "$prefix.transport.stats" || die "$direction loaded transport evidence is empty"
	set -- $(cat "$prefix.icmp.stats"); icmp_loaded_p95="$2"
	set -- $(cat "$prefix.idle.icmp.stats"); icmp_idle_p95="$2"
	set -- $(cat "$prefix.transport.stats"); transport_loaded_p95="$2"
	set -- $(cat "$prefix.idle.transport.stats"); transport_idle_p95="$2"
	icmp_delta="$(awk -v l="$icmp_loaded_p95" -v i="$icmp_idle_p95" 'BEGIN { d=l-i; if (d<0) d=0; printf "%.3f", d }')"
	transport_delta="$(awk -v l="$transport_loaded_p95" -v i="$transport_idle_p95" 'BEGIN { d=l-i; if (d<0) d=0; printf "%.3f", d }')"
	effective_delta="$(awk -v a="$icmp_delta" -v b="$transport_delta" 'BEGIN { if (a>b) printf "%.3f", a; else printf "%.3f", b }')"
	grade="$(awk -v d="$effective_delta" 'BEGIN { if (d<5) print "A+"; else if (d<30) print "A"; else if (d<60) print "B"; else if (d<200) print "C"; else if (d<400) print "D"; else print "F" }')"
	loss="$(icmp_loss_percent "$prefix.icmp.raw")"
	if [ -s "$prefix.cpu.raw" ]; then
		awk '{ print $1 }' "$prefix.cpu.raw" > "$prefix.cpu.values"
		awk '{ print $2 }' "$prefix.cpu.raw" > "$prefix.softirq.values"
		set -- $(numeric_stats "$prefix.cpu.values"); cpu_p95="$2"
		set -- $(numeric_stats "$prefix.softirq.values"); softirq_p95="$2"
	else
		cpu_p95=0; softirq_p95=0
	fi

	printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
		"$mode" "$direction" "$throughput" "$icmp_idle_p95" "$icmp_loaded_p95" "$icmp_delta" "$loss" \
		"$transport_idle_p95" "$transport_loaded_p95" "$transport_delta" "$effective_delta" "$grade" \
		"$cpu_p95" "$softirq_p95" "$background_dl" "$background_ul" "$server_id" >> "$summary_file"
	log "$direction result: ${throughput} kbit/s, grade $grade, delta ${effective_delta} ms, loss ${loss}%, CPU p95 ${cpu_p95}%, background ${background_dl}/${background_ul} kbit/s"
	sleep 3
}

run_bidirectional() {
	direction=bidirectional
	prefix="$out/$direction"
	log "capturing bidirectional idle baseline"
	capture_idle "$direction"
	prefix="$out/$direction"
	sleep 2
	reset_background_counters
	start="$(date +%s)"
	# shellcheck disable=SC2086
	"$fping_bin" -I "$wan" -D -c 300 -p 200 -- $reflectors > "$prefix.icmp.raw" 2>&1 &
	loaded_ping_pid="$!"
	transport_probe 100 500 > "$prefix.transport.raw" 2> "$prefix.transport.err" &
	loaded_transport_pid="$!"
	: > "$marker"
	cpu_monitor "$prefix.cpu.raw" &
	cpu_pid="$!"

	log "running simultaneous download and upload speedtest-go against server $server_id"
	"$speedtest" --json --unix --ping-mode http --server "$server_id" \
		--source "$source_ip" --dns-bind-source --no-upload \
		> "$prefix.download.speedtest.raw" 2> "$prefix.download.speedtest.err" &
	speed_dl_pid="$!"
	"$speedtest" --json --unix --ping-mode http --server "$server_id" \
		--source "$source_ip" --dns-bind-source --no-download \
		> "$prefix.upload.speedtest.raw" 2> "$prefix.upload.speedtest.err" &
	speed_ul_pid="$!"
	wait "$speed_dl_pid"; dl_rc="$?"
	wait "$speed_ul_pid"; ul_rc="$?"
	speed_dl_pid=""
	speed_ul_pid=""
	end="$(date +%s)"
	rm -f "$marker"
	stop_owned_pid "$loaded_ping_pid"
	stop_owned_pid "$loaded_transport_pid"
	wait "$cpu_pid" 2>/dev/null || true
	loaded_ping_pid=""
	loaded_transport_pid=""
	cpu_pid=""
	[ "$dl_rc" -eq 0 ] || die "simultaneous download speedtest-go exited $dl_rc"
	[ "$ul_rc" -eq 0 ] || die "simultaneous upload speedtest-go exited $ul_rc"

	elapsed=$((end-start)); [ "$elapsed" -gt 0 ] || elapsed=1
	dl_bytes="$(background_bytes dl)"; ul_bytes="$(background_bytes ul)"
	: "${dl_bytes:=0}"; : "${ul_bytes:=0}"
	set -- $(awk -v d="$dl_bytes" -v u="$ul_bytes" -v e="$elapsed" 'BEGIN { printf "%.3f %.3f", d*8/e/1000, u*8/e/1000 }')
	background_dl="$1"; background_ul="$2"

	dl_json="$(extract_speed_json "$prefix.download.speedtest.raw")"
	ul_json="$(extract_speed_json "$prefix.upload.speedtest.raw")"
	[ -n "$dl_json" ] || die "simultaneous download returned no JSON"
	[ -n "$ul_json" ] || die "simultaneous upload returned no JSON"
	dl_server="$($jsonfilter_bin -s "$dl_json" -e '@.servers[0].id' 2>/dev/null)"
	ul_server="$($jsonfilter_bin -s "$ul_json" -e '@.servers[0].id' 2>/dev/null)"
	[ "$dl_server" = "$server_id" ] || die "simultaneous download server changed"
	[ "$ul_server" = "$server_id" ] || die "simultaneous upload server changed"
	dl_speed_bps="$($jsonfilter_bin -s "$dl_json" -e '@.servers[0].dl_speed' 2>/dev/null)"
	ul_speed_bps="$($jsonfilter_bin -s "$ul_json" -e '@.servers[0].ul_speed' 2>/dev/null)"
	dl_throughput="$(awk -v b="$dl_speed_bps" 'BEGIN { if (b+0 <= 0) exit 1; printf "%.0f", b*8/1000 }')" ||
		die "simultaneous download returned no positive rate"
	ul_throughput="$(awk -v b="$ul_speed_bps" 'BEGIN { if (b+0 <= 0) exit 1; printf "%.0f", b*8/1000 }')" ||
		die "simultaneous upload returned no positive rate"

	extract_icmp_values "$prefix.icmp.raw" "$prefix.icmp.values"
	extract_transport_values "$prefix.transport.raw" "$prefix.transport.values"
	numeric_stats "$prefix.icmp.values" > "$prefix.icmp.stats" || die "bidirectional loaded ICMP evidence is empty"
	numeric_stats "$prefix.transport.values" > "$prefix.transport.stats" || die "bidirectional loaded transport evidence is empty"
	set -- $(cat "$prefix.icmp.stats"); icmp_loaded_p95="$2"
	set -- $(cat "$prefix.idle.icmp.stats"); icmp_idle_p95="$2"
	set -- $(cat "$prefix.transport.stats"); transport_loaded_p95="$2"
	set -- $(cat "$prefix.idle.transport.stats"); transport_idle_p95="$2"
	icmp_delta="$(awk -v l="$icmp_loaded_p95" -v i="$icmp_idle_p95" 'BEGIN { d=l-i; if (d<0) d=0; printf "%.3f", d }')"
	transport_delta="$(awk -v l="$transport_loaded_p95" -v i="$transport_idle_p95" 'BEGIN { d=l-i; if (d<0) d=0; printf "%.3f", d }')"
	effective_delta="$(awk -v a="$icmp_delta" -v b="$transport_delta" 'BEGIN { if (a>b) printf "%.3f", a; else printf "%.3f", b }')"
	grade="$(awk -v d="$effective_delta" 'BEGIN { if (d<5) print "A+"; else if (d<30) print "A"; else if (d<60) print "B"; else if (d<200) print "C"; else if (d<400) print "D"; else print "F" }')"
	loss="$(icmp_loss_percent "$prefix.icmp.raw")"
	if [ -s "$prefix.cpu.raw" ]; then
		awk '{ print $1 }' "$prefix.cpu.raw" > "$prefix.cpu.values"
		awk '{ print $2 }' "$prefix.cpu.raw" > "$prefix.softirq.values"
		set -- $(numeric_stats "$prefix.cpu.values"); cpu_p95="$2"
		set -- $(numeric_stats "$prefix.softirq.values"); softirq_p95="$2"
	else
		cpu_p95=0; softirq_p95=0
	fi

	for measured in "download:$dl_throughput" "upload:$ul_throughput"; do
		measured_direction="${measured%%:*}"
		measured_throughput="${measured#*:}"
		printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
			"$mode" "bidirectional-$measured_direction" "$measured_throughput" "$icmp_idle_p95" "$icmp_loaded_p95" "$icmp_delta" "$loss" \
			"$transport_idle_p95" "$transport_loaded_p95" "$transport_delta" "$effective_delta" "$grade" \
			"$cpu_p95" "$softirq_p95" "$background_dl" "$background_ul" "$server_id" >> "$summary_file"
	done
	log "bidirectional result: DL ${dl_throughput}, UL ${ul_throughput} kbit/s; grade $grade, delta ${effective_delta} ms, loss ${loss}%, CPU p95 ${cpu_p95}%"
}

case "${CAKE_LAB_PHASES:-directional}" in
	directional)
		run_direction download
		run_direction upload
		;;
	bidirectional)
		run_bidirectional
		;;
	all)
		run_direction download
		run_direction upload
		run_bidirectional
		;;
	*) die "invalid CAKE_LAB_PHASES value" ;;
esac

restore_runtime
[ "$cleanup_ok" -eq 1 ] || die "runtime restoration failed"
log "RESULT_DIR=$out"
cat "$summary_file"
