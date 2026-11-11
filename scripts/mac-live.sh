#!/bin/bash
# Try the main commands on a Mac against TextEdit, printing each reply. Used by CI, where the
# program has no Accessibility or Screen Recording grant, to see how far it gets.
exe="$1"
run() { echo "\$ deskcrew $*"; "$exe" "$@" 2>&1 | head -20; echo "(exit ${PIPESTATUS[0]})"; }
open -a TextEdit
sleep 3
run status
run apps
run windows
run screens
run see TextEdit
run do "type 'hello from deskcrew'" -w TextEdit
run get e1 -w TextEdit
run shot TextEdit
run clip set "deskcrew clip"
run clip
run quit
osascript -e 'tell application "TextEdit" to quit saving no' || true
