#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
init_script="$base/root/etc/init.d/cake-autorate-apply-guard"
test -x "$init_script" || {
	echo 'independent apply-guard init script is not executable' >&2
	exit 1
}
work="${TMPDIR:-/tmp}/cake-apply-guard-init-test.$$"
helper="$work/apply-guard"
log="$work/procd"
mkdir -p "$work"
trap 'rm -rf "$work"' EXIT INT TERM

cat > "$helper" <<'EOF'
#!/bin/sh
case "${APPLY_GUARD_INIT_FIXTURE:-reject}:${1:-}" in
	verified:verify-init)
		printf '%s\n' '{"state":"verified","schema_version":1,"token":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}'
		;;
	clear:verify-init) printf '%s\n' '{"state":"clear","schema_version":1}' ;;
	unsafe:verify-init) printf '%s\n' '{"state":"verified","schema_version":1,"token":"../../unsafe"}' ;;
	*) printf '%s\n' '{"state":"failed","error":"fixture rejection"}'; exit 1 ;;
esac
EOF
chmod +x "$helper"

harness() {
	state="$1"
	: > "$log"
	CAKE_AUTORATE_APPLY_GUARD="$helper"
	APPLY_GUARD_INIT_FIXTURE="$state"
	export CAKE_AUTORATE_APPLY_GUARD APPLY_GUARD_INIT_FIXTURE
	. "$init_script"
	logger() { :; }
	procd_open_instance() { printf 'instance:%s\n' "$1" >> "$log"; }
	procd_set_param() { printf 'param:%s\n' "$*" >> "$log"; }
	procd_close_instance() { printf '%s\n' close >> "$log"; }
	start_service
}

harness verified
grep -qx 'instance:transaction_aaaaaaaaaaaa' "$log"
grep -qx "param:command $helper supervise aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" "$log"
grep -qx 'param:stdout 1' "$log"
grep -qx 'param:stderr 1' "$log"
grep -qx close "$log"

for state in clear unsafe reject; do
	if harness "$state" >/dev/null 2>&1; then
		echo "independent apply-guard supervisor accepted $state" >&2
		exit 1
	fi
	[ ! -s "$log" ] || {
		echo "independent apply-guard supervisor mutated procd for $state" >&2
		exit 1
	}
done

echo 'apply guard init tests passed'
