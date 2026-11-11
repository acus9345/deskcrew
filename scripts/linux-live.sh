#!/bin/bash
# Drive a GTK window on a new virtual X display and check each reply. Run it inside a D-Bus session:
#   dbus-run-session -- bash scripts/linux-live.sh path/to/deskcrew [display number, default 99]
# It starts its own Xvfb on that display and stops only that one, and only its own test window.
# The window is the GTK one (scripts/linux-form.py) when GTK 3 is installed, or else the
# toolkit-free one, which needs `cargo build --release --example linux_form` first (or
# DESKCREW_FORM=path to it). The toolkit-free window gets its own accessibility bus, so neither
# GTK nor at-spi2-core needs to be installed; the clipboard checks are skipped without xclip/xsel.
exe="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
number="${2:-99}"
if [ -e "/tmp/.X11-unix/X$number" ] || [ -e "/tmp/.X$number-lock" ]; then
    echo "display :$number is already in use: pick another number"; exit 2
fi
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
plain="${DESKCREW_FORM:-}"
if [ -z "$plain" ] && ! python3 -c "import gi; gi.require_version('Gtk', '3.0')" 2>/dev/null; then
    plain="$(dirname "$exe")/examples/linux_form"
fi
if [ -n "$plain" ] && [ ! -x "$plain" ]; then
    echo "no GTK 3 here, and no toolkit-free test window at $plain: cargo build --release --example linux_form"; exit 2
fi
Xvfb ":$number" -screen 0 1280x800x24 >/dev/null 2>&1 &
xvfb=$!
export DISPLAY=":$number"
sleep 2
log="$(mktemp)"
bus=""
if [ -n "$plain" ]; then
    echo "--- test window: toolkit-free ($plain), on its own accessibility bus"
    started=$(dbus-daemon --session --fork --print-address=1 --print-pid=1)
    export AT_SPI_BUS_ADDRESS="$(sed -n 1p <<<"$started")"
    bus="$(sed -n 2p <<<"$started")"
    "$plain" > "$log" 2>&1 &
else
    echo "--- test window: GTK"
    python3 "$here/linux-form.py" > "$log" 2>&1 &
fi
form=$!
for _ in $(seq 50); do grep -q ready "$log" && break; sleep 0.2; done
sleep 1

expect "running" status
expect "Deskcrew Form" windows
expect "1280x800 primary" screens
expect 'edit "Name"' see "Deskcrew Form"
expect 'checkbox "Gift wrap" [off]' see "Deskcrew Form" -f
# Keys go through XTest into the focused window: select all in Name and type over it.
expect "Grace" do "click Name; key ctrl+a; type Grace" -w "Deskcrew Form"
if [ -n "$plain" ]; then
    expect "Ordered for Ada, gift wrap yes" do "set Name Ada; check 'Gift wrap'; click Submit" -w "Deskcrew Form"
else
    expect "Ordered for Ada to Japan, gift wrap yes" do "set Name Ada; select Country Japan; check 'Gift wrap'; click Submit" -w "Deskcrew Form"
    expect "=Japan" see "Deskcrew Form" -f
fi
expect "Ada Lovelace" do "type Name ' Lovelace'" -w "Deskcrew Form"
expect "Ada Lovelace" get Name -w "Deskcrew Form"
expect ".png" shot "Deskcrew Form"
if command -v xclip >/dev/null || command -v xsel >/dev/null; then
    expect "ok" clip set "deskcrew clip"
    expect "deskcrew clip" clip
else
    echo "SKIPPED the clipboard checks: neither xclip nor xsel is installed"
fi
expect "bye" quit
echo "--- what the window received:"
cat "$log"
mkdir -p shots && cp ~/.cache/deskcrew/shots/*.png shots/ 2>/dev/null
kill "$form" "$xvfb" $bus 2>/dev/null
rm -f "$log"
exit $failed
