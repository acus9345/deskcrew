"""A throwaway GTK 3 window for testing deskcrew on Linux. It prints what reaches it to stdout."""
import sys
import gi
gi.require_version('Gtk', '3.0')
gi.require_version('Gdk', '3.0')
from gi.repository import Gdk, Gio, GLib, Gtk

win = Gtk.Window(title='Deskcrew Form')
win.set_default_size(420, 640)
win.move(60, 60)
grid = Gtk.Grid(row_spacing=8, column_spacing=8, margin=12)
win.add(grid)

def named(widget, name):
    widget.get_accessible().set_name(name)
    return widget

grid.attach(Gtk.Label(label='Name'), 0, 0, 1, 1)
name = named(Gtk.Entry(), 'Name')
grid.attach(name, 1, 0, 1, 1)
grid.attach(Gtk.Label(label='Country'), 0, 1, 1, 1)
country = Gtk.ComboBoxText()
for c in ('Canada', 'Japan', 'Peru'):
    country.append_text(c)
country.set_active(0)
named(country, 'Country')
grid.attach(country, 1, 1, 1, 1)
gift = Gtk.CheckButton(label='Gift wrap')
grid.attach(gift, 1, 2, 1, 1)
result = Gtk.Label(label='No order yet')
grid.attach(result, 1, 4, 1, 1)

def submit(_):
    text = f'Ordered for {name.get_text()} to {country.get_active_text()}, gift wrap {"yes" if gift.get_active() else "no"}'
    result.set_text(text)
    print(text, flush=True)

# A pane that scrolls, which says where it got to.
notes = named(Gtk.TextView(), 'Notes')
notes.get_buffer().set_text(chr(10).join(f'line {n}' for n in range(1, 61)))
pane = Gtk.ScrolledWindow(min_content_height=80)
pane.add(notes)
pane.get_vadjustment().connect('value-changed', lambda a: print(f'scrolled to {int(a.get_value())}', flush=True))
grid.attach(pane, 1, 5, 1, 1)

# A canvas that says what the pointer did on it: each press, and each stroke with its moves.
canvas = named(Gtk.DrawingArea(), 'Canvas')
canvas.set_size_request(260, 80)
canvas.add_events(Gdk.EventMask.BUTTON_PRESS_MASK | Gdk.EventMask.BUTTON_RELEASE_MASK | Gdk.EventMask.POINTER_MOTION_MASK)
moves = [0]
def canvas_down(_, event):
    moves[0] = 0
    print(f'canvas press {event.button}', flush=True)
def canvas_move(_, event):
    if event.state & Gdk.ModifierType.BUTTON1_MASK:
        moves[0] += 1
def canvas_up(_, event):
    print(f'canvas stroke {moves[0]} moves', flush=True)
canvas.connect('button-press-event', canvas_down)
canvas.connect('motion-notify-event', canvas_move)
canvas.connect('button-release-event', canvas_up)
grid.attach(canvas, 1, 6, 1, 1)

# A small table, for reading rows.
stock = Gtk.ListStore(str, int)
for fruit, count in (('Apples', 12), ('Pears', 7), ('Plums', 30)):
    stock.append([fruit, count])
table = named(Gtk.TreeView(model=stock), 'Stock')
for at, title in enumerate(('Fruit', 'Count')):
    table.append_column(Gtk.TreeViewColumn(title, Gtk.CellRendererText(), text=at))
grid.attach(table, 1, 7, 1, 1)

# A file dialog, which says what was picked.
def open_file(_):
    dialog = Gtk.FileChooserDialog(title='Open File', transient_for=win, action=Gtk.FileChooserAction.OPEN)
    dialog.add_buttons('_Cancel', Gtk.ResponseType.CANCEL, '_Open', Gtk.ResponseType.ACCEPT)
    def answered(d, response):
        if response == Gtk.ResponseType.ACCEPT:
            print(f'picked {d.get_filename()}', flush=True)
        d.destroy()
    dialog.connect('response', answered)
    dialog.show()
opener = Gtk.Button(label='Open file')
opener.connect('clicked', open_file)
grid.attach(opener, 1, 8, 1, 1)

# Stand in for the desktop's notification server, so a notification can be sent on a bare bus.
NOTIFY = '''<node><interface name="org.freedesktop.Notifications">
<method name="Notify"><arg type="s" direction="in"/><arg type="u" direction="in"/><arg type="s" direction="in"/>
<arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="as" direction="in"/><arg type="a{sv}" direction="in"/>
<arg type="i" direction="in"/><arg type="u" direction="out"/></method></interface></node>'''
def notified(conn, sender, path, iface, method, args, invocation):
    invocation.return_value(GLib.Variant('(u)', (1,)))
def got_bus(conn, name):
    conn.register_object('/org/freedesktop/Notifications', Gio.DBusNodeInfo.new_for_xml(NOTIFY).interfaces[0], notified, None, None)
try:
    Gio.bus_own_name(Gio.BusType.SESSION, 'org.freedesktop.Notifications', Gio.BusNameOwnerFlags.NONE, got_bus, None, None)
except GLib.Error:
    pass

button = Gtk.Button(label='Submit')
button.connect('clicked', submit)
grid.attach(button, 1, 3, 1, 1)
win.connect('destroy', Gtk.main_quit)
win.show_all()
print('ready', flush=True)
Gtk.main()
