#!/bin/sh
set -eu

package="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
repo="$(CDPATH= cd -- "$package/../.." && pwd)"
main_init="$package/files/etc/init.d/cake-autorate"
lock="$package/files/usr/libexec/cake-autorate-rs/runtime-lock"
calibration_init="$package/files/etc/init.d/cake-autorate-autotune"

actual="$({
	find "$package/files" "$repo/package/luci-app-cake-autorate-rs/root" \
		-type f -exec sh -c '
			for file do
				IFS= read -r first <"$file" || first=""
				case "$first" in "#!"*) printf "%s\n" "$file" ;; esac
			done
		' sh {} +
} | LC_ALL=C sort)"
expected="$(printf '%s\n' "$main_init" "$lock" "$calibration_init" | LC_ALL=C sort)"
[ "$actual" = "$expected" ] || {
	echo 'unexpected shipped shell executable surface:' >&2
	printf 'expected:\n%s\nactual:\n%s\n' "$expected" "$actual" >&2
	exit 1
}

for script in "$main_init" "$lock" "$calibration_init"; do
	if grep -Eq '(^|[;&|[:space:]])(uci|tc|ip|nft|ubus|jsonfilter|sleep|usleep|wget|curl|ping|fping|speedtest|logread)([;&|[:space:]]|$)' "$script"; then
		echo "shipped bridge retained business command authority: $script" >&2
		exit 1
	fi
done

if grep -Eq 'PKG_UPGRADE|NATIVE_APPLY_RECOVERY_ROOT|native Apply recovery transaction is pending' "$main_init"; then
	echo 'main rc.common bridge retained package or Apply-marker policy' >&2
	exit 1
fi
grep -Fq '"$DAEMON" --service-lifecycle prepare-start' "$main_init"
grep -Fq '"$DAEMON" --service-lifecycle execute-stop' "$main_init"
grep -Fq '"$DAEMON" --service-lifecycle confirm-started' "$main_init"

if grep -Eq 'config_(load|get)|autotune_scheduler_engine|--native-apply-recover|--legacy-apply-recover|--legacy-autotune-recover|--scheduler-adopt-legacy|--calibration-capabilities|sleep|usleep' "$calibration_init"; then
	echo 'calibration rc.common bridge retained recovery or scheduler policy' >&2
	exit 1
fi
grep -Fq '"$DAEMON" --calibration-service prepare-start' "$calibration_init"
grep -Fq '"$DAEMON" --calibration-service prepare-stop' "$calibration_init"
grep -Fq '"$DAEMON" --calibration-service confirm-started' "$calibration_init"
grep -Fq '"$DAEMON" --calibration-service confirm-stopped' "$calibration_init"
grep -Fq 'procd_set_param command "$DAEMON" --calibrationd' "$calibration_init"

if find "$repo/package/luci-app-cake-autorate-rs/root/usr/libexec" \
	-type f -print -quit 2>/dev/null | grep -q .; then
	echo 'LuCI payload regained a legacy executable helper' >&2
	exit 1
fi

printf '%s\n' 'shipped shell bridge boundary tests passed'
