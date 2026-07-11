#!/bin/sh

set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
PACKAGE_DIR="$SCRIPT_DIR/packages"

if [ ! -f "$PACKAGE_DIR/packages.adb" ]; then
	echo "Offline APK repository not found: $PACKAGE_DIR" >&2
	exit 1
fi

exec apk add --allow-untrusted --no-network \
	--repository "file://$PACKAGE_DIR/packages.adb" \
	--upgrade \
	cake-autorate-rs luci-app-cake-autorate-rs
