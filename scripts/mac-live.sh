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
expect "bye" quit
pkill -x TextEdit || true
exit $failed
