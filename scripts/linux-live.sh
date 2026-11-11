#!/bin/bash
# Drive a GTK window on a virtual X display and check each reply. Run it inside a D-Bus session:
#   dbus-run-session -- bash scripts/linux-live.sh path/to/deskcrew
exe="$1"
failed=0
here="$(cd "$(dirname "$0")" && pwd)"
run() { echo "\$ deskcrew $*"; out=$("$exe" "$@" 2>&1); code=$?; echo "$out" | head -30; echo "(exit $code)"; }
expect() { # expect "text" command...
    want="$1"; shift
    run "$@"
    if [ $code -ne 0 ] || ! grep -qF -- "$want" <<<"$out"; then
        echo "FAILED: expected '$want'"; failed=1
    fi
}
refuse() { # refuse "text" command...: it must fail and say this
    want="$1"; shift
    run "$@"
    if [ $code -eq 0 ] || ! grep -qF -- "$want" <<<"$out"; then
        echo "FAILED: expected a refusal with '$want'"; failed=1
    fi
}

# A Wayland session with no X display gets a plain refusal.
echo "\$ WAYLAND_DISPLAY=wayland-0 deskcrew windows (no DISPLAY)"
out=$(env -u DISPLAY WAYLAND_DISPLAY=wayland-0 "$exe" windows 2>&1); code=$?
echo "$out"; echo "(exit $code)"
if [ $code -eq 0 ] || ! grep -qF "Wayland" <<<"$out"; then echo "FAILED: expected a Wayland refusal"; failed=1; fi
Xvfb :99 -screen 0 1280x800x24 >/dev/null 2>&1 &
export DISPLAY=:99
sleep 2
python3 "$here/linux-form.py" > /tmp/form.log 2>&1 &
for _ in $(seq 50); do grep -q ready /tmp/form.log && break; sleep 0.2; done
sleep 1

expect "running" status
expect "Deskcrew Form" windows
expect "1280x800 primary" screens
expect 'edit "Name"' see "Deskcrew Form"
expect 'checkbox "Gift wrap" [off]' see "Deskcrew Form" -f
# Keys go through XTest into the focused window: select all in Name and type over it.
expect "Grace" do "click Name; key ctrl+a; type Grace" -w "Deskcrew Form"
expect "Ordered for Ada to Japan, gift wrap yes" do "set Name Ada; select Country Japan; check 'Gift wrap'; click Submit" -w "Deskcrew Form"
expect "=Japan" see "Deskcrew Form" -f
expect "Ada Lovelace" do "type Name ' Lovelace'" -w "Deskcrew Form"
expect "Ada Lovelace" get Name -w "Deskcrew Form"
expect ".png" shot "Deskcrew Form"
expect "ok" clip set "deskcrew clip"
expect "deskcrew clip" clip
expect "bye" quit
echo "--- what the window received:"
cat /tmp/form.log
mkdir -p shots && cp ~/.cache/deskcrew/shots/*.png shots/ 2>/dev/null
pkill -f linux-form.py
pkill Xvfb
exit $failed
