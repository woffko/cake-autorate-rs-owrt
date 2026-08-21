#!/bin/sh
set -eu

[ "$#" -eq 3 ] || {
	printf '%s\n' "usage: render-init-variant.sh full|lite INPUT OUTPUT" >&2
	exit 2
}

variant="$1"
input="$2"
output="$3"
case "$variant" in
	full|lite) ;;
	*)
		printf '%s\n' "unsupported init variant: $variant" >&2
		exit 2
		;;
esac

[ -f "$input" ] && [ ! -L "$input" ] || {
	printf '%s\n' "init template is not a regular file: $input" >&2
	exit 1
}

umask 022
tmp="${output}.tmp.$$"
trap 'rm -f "$tmp"' EXIT HUP INT TERM

awk -v wanted="$variant" '
	BEGIN { section = "shared"; ok = 1 }
	/^[[:space:]]*# @variant (full|lite) begin$/ {
		if (section != "shared") {
			print "nested init variant marker at line " NR > "/dev/stderr"
			ok = 0
			exit 1
		}
		section = $3
		next
	}
	/^[[:space:]]*# @variant (full|lite) end$/ {
		if (section != $3) {
			print "mismatched init variant marker at line " NR > "/dev/stderr"
			ok = 0
			exit 1
		}
		section = "shared"
		next
	}
	section == "shared" || section == wanted { print }
	END {
		if (ok && section != "shared") {
			print "unterminated init variant marker" > "/dev/stderr"
			exit 1
		}
	}
' "$input" >"$tmp"

chmod 0755 "$tmp"
mv -f "$tmp" "$output"
trap - EXIT HUP INT TERM
