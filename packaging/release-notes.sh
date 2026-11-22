#!/usr/bin/env bash
# Prints the release description of <version> from CHANGELOG.md.
#
#   packaging/release-notes.sh 0.1.0-beta > notes.md
set -euo pipefail

version="${1:?usage: packaging/release-notes.sh <version>}"
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"

changes="$(awk -v heading="## $version" '
    /^## / {
        if (found) exit
        found = ($0 == heading || index($0, heading " ") == 1)
        next
    }
    found
' "$root/CHANGELOG.md" | sed '/./,$!d' | awk '
    function flush() { if (have) print line; have = 0 }
    /^$/ { flush(); print ""; next }
    /^(#|- |[0-9]+\. |\||>)/ { flush(); line = $0; have = 1; next }
    { sub(/^ +/, ""); if (have) line = line " " $0; else { line = $0; have = 1 } }
    END { flush() }
')"
if [[ -z "${changes//[[:space:]]/}" ]]; then
    echo "CHANGELOG.md has no section for $version" >&2
    exit 1
fi

kind='Release'
[[ "$version" == *-* ]] && kind='Prerelease'

cat <<NOTES
$kind for Linux (Ubuntu 24.04+, Debian 13+, amd64).

- **awg-gui_${version}_amd64.deb** — Debian/Ubuntu package: \`sudo apt install ./awg-gui_${version}_amd64.deb\` installs **AmneziaWG** with its menu entry, icons and polkit policy.

\`amneziawg-go\`, \`awg\` and \`awg-quick\` are built into the program: no kernel module or other AmneziaWG packages are needed.

$changes

Tunnel configs are stored in \`~/.config/awg-gui/tunnels/\`. Please report problems on GitHub Issues with the text from the **Log** tab.

License: GNU GPL v2 or later. File checksums are in \`SHA256SUMS.txt\`.
NOTES
