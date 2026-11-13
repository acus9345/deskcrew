#!/bin/bash
# Drive TextEdit on a Mac and check each reply. GitHub's Mac runners allow Accessibility, so the
# reading and typing checks must pass; a picture also needs Screen Recording, so `shot` is only
# printed.
exe="$1"
failed=0
run() { echo "\$ deskcrew $*"; out=$("$exe" "$@" 2>&1); code=$?; echo "$out" | head -25; echo "(exit $code)"; }
expect() { # expect "text" command...
    want="$1"; shift
    run "$@"
    if [ $code -ne 0 ] || ! grep -qF -- "$want" <<<"$out"; then
        echo "FAILED: expected '$want'"; failed=1
    fi
}
open -a TextEdit
sleep 3
expect "running" status
expect "TextEdit" apps
expect "TextEdit" windows
expect "primary" screens
expect "First Text View" see TextEdit
expect "from deskcrew" do "type 'hello from deskcrew'" -w TextEdit
# Typing opens untitled panels in TextEdit: the window being worked in must still be the one picked.
expect "from deskcrew" get e1 -w TextEdit
run shot TextEdit
expect "ok" clip set "deskcrew clip"
expect "deskcrew clip" clip
# A picture on the clipboard is still there after an agent pastes text.
picture="$(mktemp -t clip).png"
python3 -c "
import struct, zlib, sys
def chunk(kind, data): return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data) & 0xffffffff)
rows = b''.join(b'\x00' + bytes([208, 111, 89]) * 8 for _ in range(8))
open(sys.argv[1], 'wb').write(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', 8, 8, 8, 2, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(rows)) + chunk(b'IEND', b''))
" "$picture"
board="ObjC.import('AppKit'); var board = \$.NSPasteboard.generalPasteboard;"
osascript -l JavaScript -e "$board board.clearContents; board.setDataForType(\$.NSData.dataWithContentsOfFile('$picture'), 'public.png')"
before=$(osascript -l JavaScript -e "$board board.dataForType('public.png').length" 2>&1)
echo "picture on the clipboard before the paste: $before bytes"
expect "paste" type " pasted" --paste -w TextEdit
expect "pasted" get e1 -w TextEdit
after=$(osascript -l JavaScript -e "$board board.dataForType('public.png').length" 2>&1)
echo "picture on the clipboard after the paste: $after bytes"
if [ -z "$before" ] || [ "$before" = "0" ] || [ "$after" != "$before" ]; then echo "FAILED: the clipboard picture was not put back"; failed=1; fi
expect "bye" quit
pkill -x TextEdit || true
exit $failed
