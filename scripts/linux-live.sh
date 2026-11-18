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

# With neither X11 nor Wayland, deskcrew refuses to run.
echo "\$ deskcrew windows (no DISPLAY or WAYLAND_DISPLAY)"
out=$(env -u DISPLAY -u WAYLAND_DISPLAY "$exe" windows 2>&1); code=$?
echo "$out"; echo "(exit $code)"
if [ $code -eq 0 ] || ! grep -qF "no X display" <<<"$out"; then echo "FAILED: expected a no-display refusal"; failed=1; fi

# A Wayland session with no X display refuses X11-only commands such as shot with a clear error.
wayland_test="deskcrew-live-$$"
echo "\$ WAYLAND_DISPLAY=$wayland_test deskcrew shot --screen (no DISPLAY)"
out=$(env -u DISPLAY WAYLAND_DISPLAY="$wayland_test" "$exe" shot --screen 2>&1); code=$?
echo "$out"; echo "(exit $code)"
if [ $code -eq 0 ] || ! grep -qF "Wayland" <<<"$out"; then echo "FAILED: expected a Wayland refusal on shot"; failed=1; fi
env -u DISPLAY WAYLAND_DISPLAY="$wayland_test" "$exe" quit >/dev/null 2>&1 || true
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
mkdir -p shots

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
if [ -z "$plain" ]; then
    # Scrolling moves the pane's scroll bar through accessibility: no pointer, no raising.
    expect "scroll bar" scroll Notes down 2 -w "Deskcrew Form"
fi
if command -v xclip >/dev/null || command -v xsel >/dev/null; then
    expect "ok" clip set "deskcrew clip"
    expect "deskcrew clip" clip
fi
picture="$(ls ~/.cache/deskcrew/shots/*.png 2>/dev/null | head -1)"
if command -v xclip >/dev/null && [ -n "$picture" ]; then
    # A picture on the clipboard is still there after an agent pastes text.
    xclip -selection clipboard -t image/png -i "$picture" &
    sleep 0.5
    expect "paste" type Name " pasted" --paste -w "Deskcrew Form"
    # The toolkit-free window has no clipboard of its own, so only GTK takes the paste.
    if [ -z "$plain" ]; then
        expect "pasted" get Name -w "Deskcrew Form"
    fi
    sleep 0.5
    if xclip -selection clipboard -t image/png -o 2>/dev/null | cmp -s - "$picture"; then
        echo "the clipboard picture came back"
    else
        echo "FAILED: the clipboard picture was not put back"; failed=1
    fi
elif ! command -v xclip >/dev/null && ! command -v xsel >/dev/null; then
    echo "SKIPPED the clipboard checks: neither xclip nor xsel is installed"
fi
# Commands the Linux version gained: tables, file dialogs and notifications (GTK window only).
if [ -z "$plain" ]; then
    expect "3 rows, 2 columns" table Stock -w "Deskcrew Form"
    expect "Plums	30" table Stock -w "Deskcrew Form"
    expect "ok" click "Open file" -w "Deskcrew Form"
    sleep 1
    expect "dialog closed" pick-file /etc/hostname -w "Deskcrew Form"
    sleep 0.5
    if ! grep -qF "picked /etc/hostname" "$log"; then
        echo "FAILED: the file dialog did not get the path"; failed=1
        run windows
        run see "Open File" -f
    fi
    python3 -c "
from gi.repository import Gio, GLib
bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
bus.call_sync('org.freedesktop.Notifications', '/org/freedesktop/Notifications', 'org.freedesktop.Notifications', 'Notify',
    GLib.Variant('(susssasa{sv}i)', ('deskcrew-test', 0, '', 'Build finished', 'all checks passed', [], {}, 5000)), None, Gio.DBusCallFlags.NONE, 2000, None)
" || echo "(sending the test notification failed)"
    sleep 0.5
    expect "Build finished: all checks passed" notifications
fi
# Pointer work goes by the agents' own X pointer, so the person's stays where it is, and nothing
# is brought forward while the window shows at the point.
if command -v xdotool >/dev/null; then
    xdotool mousemove 7 7
    if [ -n "$plain" ]; then
        # The toolkit-free window flips Gift wrap on a press of any button inside it.
        expect "own pointer" click 'Gift wrap' --right -w "Deskcrew Form"
        expect 'checkbox "Gift wrap" [off]' see "Deskcrew Form" -f
    else
        expect "own pointer" drag Canvas Notes -w "Deskcrew Form"
        sleep 0.5
        if ! grep -qE "canvas stroke [1-9][0-9]* moves" "$log"; then echo "FAILED: the canvas got no stroke from the agents' pointer"; failed=1; fi
        expect "own pointer" click Canvas --right -w "Deskcrew Form"
        sleep 0.3
        if ! grep -qF "canvas press 3" "$log"; then echo "FAILED: the canvas got no right press from the agents' pointer"; failed=1; fi
    fi
    where=$(xdotool getmouselocation | cut -d' ' -f1-2)
    echo "the person's pointer: $where"
    if [ "$where" != "x:7 y:7" ]; then echo "FAILED: the person's pointer moved to $where"; failed=1; fi
else
    echo "SKIPPED the own pointer checks: xdotool is not installed"
fi
# Each agent's drawn cursor is on the screen, in its colour: Claude's is 217,119,87.
if command -v xwd >/dev/null; then
    run click Name -w "Deskcrew Form" --agent Claude
    if [ $code -ne 0 ]; then echo "FAILED: the click for the cursor check"; failed=1; fi
    sleep 0.3
    cursor=$(xwd -root -silent | python3 "$here/xwd-colour.py" 217 119 87)
    echo "pixels in Claude's colour on the screen: $cursor"
    if [ "${cursor:-0}" -lt 40 ]; then echo "FAILED: expected Claude's drawn cursor on the screen"; failed=1; fi
    xwd -root -silent > shots/screen-with-cursor.xwd 2>/dev/null || true
    # Typing into a named field turns the agent's cursor into a caret beneath its tag.
    expect "!" type Name "!" -w "Deskcrew Form" --agent Claude
    sleep 1
    caret=$(xwd -root -silent | python3 "$here/xwd-colour.py" --caret 217 119 87)
    echo "caret bar rows in Claude's colour below the tag: $caret"
    if [ "${caret:-0}" -lt 10 ]; then echo "FAILED: expected Claude's typing caret on the screen"; failed=1; fi
    xwd -root -silent > shots/screen-with-caret.xwd 2>/dev/null || true
else
    echo "SKIPPED the drawn cursor and caret checks: xwd is not installed"
fi
expect "bye" quit

# Pure Wayland mode (no DISPLAY): AT-SPI reading and control actions work, while X11-only commands refuse clearly.
wrun() { echo "\$ WAYLAND_DISPLAY=$wayland_test deskcrew $*"; out=$(env -u DISPLAY WAYLAND_DISPLAY="$wayland_test" "$exe" "$@" 2>&1); code=$?; echo "$out" | head -30; echo "(exit $code)"; }
wexpect() { want="$1"; shift; wrun "$@"; if [ $code -ne 0 ] || ! grep -qF -- "$want" <<<"$out"; then echo "FAILED (Wayland): expected '$want'"; failed=1; fi; }
wrefuse() { want="$1"; shift; wrun "$@"; if [ $code -eq 0 ] || ! grep -qF -- "$want" <<<"$out"; then echo "FAILED (Wayland): expected a refusal with '$want'"; failed=1; fi; }

wexpect "Deskcrew Form" windows
wexpect 'edit "Name"' see "Deskcrew Form" -f
wexpect "Ada" get Name -w "Deskcrew Form"
if [ -n "$plain" ]; then
    wexpect "Ordered for Grace, gift wrap yes" do "set Name Grace; check 'Gift wrap'; click Submit" -w "Deskcrew Form"
else
    wexpect "Ordered for Grace to Peru, gift wrap yes" do "set Name Grace; select Country Peru; uncheck 'Gift wrap'; check 'Gift wrap'; click Submit" -w "Deskcrew Form"
fi
wexpect "Grace" get Name -w "Deskcrew Form"
wrefuse "Wayland" shot "Deskcrew Form"
wrefuse "Wayland" key enter -w "Deskcrew Form"
wrefuse "Wayland" focus -w "Deskcrew Form"
wrefuse "Wayland" screens
wexpect "bye" quit
echo "--- what the window received:"
cat "$log"
mkdir -p shots && cp ~/.cache/deskcrew/shots/*.png shots/ 2>/dev/null
kill "$form" "$xvfb" $bus 2>/dev/null
rm -f "$log"
exit $failed
