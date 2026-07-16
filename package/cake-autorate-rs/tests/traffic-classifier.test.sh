#!/bin/sh
set -eu

ROOT="$(mktemp -d)"
trap 'rm -rf "$ROOT"' EXIT
mkdir -p "$ROOT/bin" "$ROOT/sys/eth0" "$ROOT/runtime"

cat > "$ROOT/bin/uci" <<'EOF'
#!/bin/sh
case "$*" in
	"-q show cake-autorate")
		printf '%s\n' \
			"cake-autorate.wan_sqm=cake_autorate" \
			"cake-autorate.rule1=traffic_rule" \
			"cake-autorate.rule_other=traffic_rule"
		;;
	"-q get cake-autorate.wan_sqm.enabled") printf '%s\n' "${TC_INSTANCE_ENABLED:-1}" ;;
	"-q get cake-autorate.wan_sqm.manage_sqm") printf '%s\n' "${TC_MANAGE_SQM:-1}" ;;
	"-q get cake-autorate.wan_sqm.sqm_enabled") printf '%s\n' "${TC_SQM_ENABLED:-1}" ;;
	"-q get cake-autorate.wan_sqm.traffic_rules_enabled") printf '%s\n' "${TC_RULES_ENABLED-1}" ;;
	"-q get cake-autorate.wan_sqm.autotune_profile") printf '%s\n' "${TC_PROFILE:-gaming}" ;;
	"-q get cake-autorate.wan_sqm.sqm_script") printf '%s\n' "${TC_SQM_SCRIPT:-layer_cake.qos}" ;;
	"-q get cake-autorate.wan_sqm.sqm_eqdisc_opts") printf '%s\n' "${TC_SQM_EQDISC_OPTS:-diffserv4}" ;;
	"-q get cake-autorate.wan_sqm.wan_if") printf 'eth0\n' ;;
	"-q get cake-autorate.wan_sqm.sqm_interface") printf 'eth0\n' ;;
	"-q get cake-autorate.wan_sqm.ul_if") printf 'eth0\n' ;;
	"-q get cake-autorate.wan_sqm.traffic_defaults_gaming")
		printf '%s\n' "${TC_DEFAULTS_GAMING:-1}"
		;;
	"-q get cake-autorate.wan_sqm.traffic_defaults_best_overall")
		printf '%s\n' "${TC_DEFAULTS_BEST:-1}"
		;;
	"-q get cake-autorate.wan_sqm.traffic_defaults_fair")
		printf '%s\n' "${TC_DEFAULTS_FAIR:-1}"
		;;
	"-q get cake-autorate.rule1.enabled") printf '%s\n' "${TC_CUSTOM_ENABLED:-1}" ;;
	"-q get cake-autorate.rule1.instance") printf 'wan_sqm\n' ;;
	"-q get cake-autorate.rule1.profile") printf '%s\n' "${TC_CUSTOM_PROFILE:-gaming}" ;;
	"-q get cake-autorate.rule1.preset") printf '%s\n' "${TC_CUSTOM_PRESET:-wireguard}" ;;
	"-q get cake-autorate.rule1.protocol") printf '%s\n' "${TC_CUSTOM_PROTOCOL:-udp}" ;;
	"-q get cake-autorate.rule1.family") printf '%s\n' "${TC_CUSTOM_FAMILY:-any}" ;;
	"-q get cake-autorate.rule1.source_ports") printf '%s\n' "${TC_CUSTOM_SOURCE_PORTS:-}" ;;
	"-q get cake-autorate.rule1.destination_ports") printf '%s\n' "${TC_CUSTOM_DESTINATION_PORTS:-}" ;;
	"-q get cake-autorate.rule1.source_network") printf '%s\n' "${TC_CUSTOM_SOURCE_NETWORK:-192.168.1.50/32}" ;;
	"-q get cake-autorate.rule1.destination_network") printf '%s\n' "${TC_CUSTOM_DESTINATION_NETWORK:-}" ;;
	"-q get cake-autorate.rule1.class") printf '%s\n' "${TC_CUSTOM_CLASS:-video}" ;;
	"-q get cake-autorate.rule1.order") printf '%s\n' "${TC_CUSTOM_ORDER:-100}" ;;
	"-q get cake-autorate.rule_other.enabled") printf '1\n' ;;
	"-q get cake-autorate.rule_other.instance") printf 'wan_sqm\n' ;;
	"-q get cake-autorate.rule_other.profile") printf 'fair\n' ;;
	"-q get cake-autorate.rule_other.preset") printf 'dns\n' ;;
	"-q get cake-autorate.rule_other.protocol") printf 'udp\n' ;;
	"-q get cake-autorate.rule_other.family") printf 'any\n' ;;
	"-q get cake-autorate.rule_other.source_ports") printf '\n' ;;
	"-q get cake-autorate.rule_other.destination_ports") printf '\n' ;;
	"-q get cake-autorate.rule_other.source_network") printf '\n' ;;
	"-q get cake-autorate.rule_other.destination_network") printf '\n' ;;
	"-q get cake-autorate.rule_other.class") printf 'background\n' ;;
	"-q get cake-autorate.rule_other.order") printf '200\n' ;;
	*) exit 1 ;;
esac
EOF

cat > "$ROOT/bin/nft" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >> "$TC_NFT_LOG"
case "$*" in
	"list table inet cake_autorate_dscp")
		[ -f "$TC_TABLE_MARKER" ]
		;;
	"-j list table inet cake_autorate_dscp")
		[ -f "$TC_TABLE_MARKER" ] || exit 1
		if [ "${TC_NFT_DRIFT:-0}" = 1 ]; then
			printf '{"nftables":[{"drifted":true}]}\n'
		else
			cat "$TC_NFT_APPLIED"
		fi
		;;
	"-c -f "*)
		cp "$3" "$TC_NFT_CHECKED"
		;;
	"-f "*)
		cp "$2" "$TC_NFT_APPLIED"
		touch "$TC_TABLE_MARKER"
		;;
	"delete table inet cake_autorate_dscp")
		rm -f "$TC_TABLE_MARKER"
		;;
	*) : ;;
esac
EOF

chmod +x "$ROOT/bin/uci" "$ROOT/bin/nft"

HELPER="$(dirname "$0")/../files/usr/libexec/cake-autorate-rs/traffic-classifier"
export CAKE_AUTORATE_UCI_BIN="$ROOT/bin/uci"
export CAKE_AUTORATE_NFT_BIN="$ROOT/bin/nft"
export CAKE_AUTORATE_UBUS_BIN="/bin/false"
export CAKE_AUTORATE_JSONFILTER_BIN="/bin/false"
export CAKE_AUTORATE_SYS_CLASS_NET="$ROOT/sys"
export CAKE_AUTORATE_RUNTIME_ROOT="$ROOT/runtime"
export TC_NFT_LOG="$ROOT/nft.log"
export TC_NFT_CHECKED="$ROOT/checked.nft"
export TC_NFT_APPLIED="$ROOT/applied.nft"
export TC_TABLE_MARKER="$ROOT/table-present"

"$HELPER" render > "$ROOT/gaming.nft"
grep -q '^table inet cake_autorate_dscp {' "$ROOT/gaming.nft"
[ "$(grep -c 'oifname "eth0" ip dscp set cs0' "$ROOT/gaming.nft")" -eq 2 ]
[ "$(grep -c 'udp dport { 27000-27100 } ip dscp set cs5' "$ROOT/gaming.nft")" -eq 2 ]
[ "$(grep -c 'udp dport { 88,3074 } ip dscp set cs5' "$ROOT/gaming.nft")" -eq 2 ]
[ "$(grep -c 'ip saddr 192.168.1.50/32 meta l4proto udp udp dport { 51820 } ip dscp set af41' "$ROOT/gaming.nft")" -eq 2 ]
if grep -q 'dscp set cs1' "$ROOT/gaming.nft"; then
	echo "inactive Fair rule leaked into the Gaming profile" >&2
	exit 1
fi

export TC_RULES_ENABLED=''
"$HELPER" render > "$ROOT/upgrade-opt-in.nft"
if grep -q 'oifname "eth0"' "$ROOT/upgrade-opt-in.nft"; then
	echo "an upgraded instance without an explicit rule opt-in was modified" >&2
	exit 1
fi
export TC_RULES_ENABLED=1

export TC_DEFAULTS_GAMING=0
"$HELPER" render > "$ROOT/custom-only.nft"
if grep -q '27000-27100\|dport { 53 }' "$ROOT/custom-only.nft"; then
	echo "disabled built-in defaults were still rendered" >&2
	exit 1
fi
grep -q 'dport { 51820 } ip dscp set af41' "$ROOT/custom-only.nft"

export TC_DEFAULTS_GAMING=1
: > "$TC_NFT_LOG"
"$HELPER" apply > "$ROOT/apply.json"
grep -q '"state":"active"' "$ROOT/apply.json"
grep -q '"schema_version":2' "$ROOT/apply.json"
grep -q '"instances":1' "$ROOT/apply.json"
grep -q '"custom_rules":1' "$ROOT/apply.json"
grep -Eq '"ruleset_sha256":"[0-9a-f]{64}"' "$ROOT/apply.json"
grep -q '^-c -f ' "$TC_NFT_LOG"
grep -q '^-f ' "$TC_NFT_LOG"
cmp "$TC_NFT_CHECKED" "$TC_NFT_APPLIED"
grep -q 'table inet cake_autorate_dscp' "$TC_NFT_APPLIED"
"$HELPER" status > "$ROOT/status-active.json"
grep -q '"state":"active"' "$ROOT/status-active.json"
grep -q '"instances":1' "$ROOT/status-active.json"
grep -q '"attested_instances":"wan_sqm|eth0|gaming"' "$ROOT/status-active.json"
"$HELPER" status wan_sqm > "$ROOT/status-instance.json"
grep -q '"state":"active"' "$ROOT/status-instance.json"
grep -q '"target":"eth0"' "$ROOT/status-instance.json"
grep -q '"profile":"gaming"' "$ROOT/status-instance.json"
"$HELPER" status missing_instance > "$ROOT/status-missing.json"
grep -q '"state":"missing"' "$ROOT/status-missing.json"
export TC_NFT_DRIFT=1
"$HELPER" status wan_sqm > "$ROOT/status-drifted.json"
grep -q '"state":"drifted"' "$ROOT/status-drifted.json"
export TC_NFT_DRIFT=0

export TC_CUSTOM_PRESET=custom
export TC_CUSTOM_DESTINATION_PORTS='53;delete-table'
if "$HELPER" render >/dev/null 2>&1; then
	echo "unsafe custom port expression was accepted" >&2
	exit 1
fi
export TC_CUSTOM_DESTINATION_PORTS=''
export TC_CUSTOM_PRESET=wireguard

export TC_MANAGE_SQM=0
"$HELPER" render > "$ROOT/unmanaged.nft"
if grep -q 'oifname "eth0"' "$ROOT/unmanaged.nft"; then
	echo "an unmanaged SQM instance received traffic-priority rules" >&2
	exit 1
fi
export TC_MANAGE_SQM=1

export TC_SQM_SCRIPT=piece_of_cake.qos
export TC_SQM_EQDISC_OPTS=''
"$HELPER" render > "$ROOT/legacy-besteffort.nft"
if grep -q 'oifname "eth0"' "$ROOT/legacy-besteffort.nft"; then
	echo "rules were emitted for an upload CAKE queue that cannot consume diffserv4" >&2
	exit 1
fi
export TC_SQM_SCRIPT=layer_cake.qos
export TC_SQM_EQDISC_OPTS=diffserv4

export TC_INSTANCE_ENABLED=0
: > "$TC_NFT_LOG"
"$HELPER" apply > "$ROOT/inactive.json"
grep -q '"state":"inactive"' "$ROOT/inactive.json"
grep -q '^delete table inet cake_autorate_dscp$' "$TC_NFT_LOG"

"$HELPER" status > "$ROOT/status.json"
grep -q '"state":"inactive"' "$ROOT/status.json"
grep -q '"table_present":false' "$ROOT/status.json"

"$HELPER" clear > "$ROOT/clear.json"
grep -q '"state":"inactive"' "$ROOT/clear.json"

echo "traffic-classifier tests passed"
