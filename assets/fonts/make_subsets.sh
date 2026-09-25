#!/usr/bin/env bash
# Regenerate the embedded CJK font subset (needs fontTools: `python -m pip install fonttools`).
#
#   assets/fonts/make_subsets.sh [path/to/NotoSansSC-Regular.ttf]
#
# Source font (OFL-1.1), static TrueType from the Google Fonts CSS API:
#   https://fonts.gstatic.com/s/notosanssc/v40/k3kCo84MPvpLmixcA63oeAL7Iqp5IZJF9bmaG9_FnYw.ttf
# The "ui" set = ASCII + Latin-1 + CJK punctuation + fullwidth forms + CAD symbols
#                + 通用规范汉字表 level 1 (3500) ∪ GB2312 level 1 (3755) = 4515 characters.
# Do not name the subset "Source ..." (Reserved Font Name under the OFL).
set -euo pipefail
cd "$(dirname "$0")"
PY=${PY:-python}
SRC=${1:-NotoSansSC-Regular.ttf}
if [ ! -f "$SRC" ]; then
  echo "source font not found: $SRC (download it from the URL in this script's header)" >&2
  exit 1
fi
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
# U+25FB is egui's primary replacement glyph.
cat charsets/set_ui.txt > "$TMP/cs_ui.txt"
printf '%s' $'◻' >> "$TMP/cs_ui.txt"
"$PY" -m fontTools.subset "$SRC" --text-file="$TMP/cs_ui.txt" --output-file=NotoSansSC-Regular-ui.ttf \
  --no-hinting --name-IDs='*' --name-languages='*' \
  --layout-features='kern,liga,ccmp,locl,mark,mkmk,vert,fwid,hwid,halt,palt' \
  --drop-tables+=vhea,vmtx,BASE,STAT
printf 'NotoSansSC-Regular-ui.ttf raw=%s gz9=%s\n' "$(wc -c < NotoSansSC-Regular-ui.ttf)" "$(gzip -9 -c NotoSansSC-Regular-ui.ttf | wc -c)"
