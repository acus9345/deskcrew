"""A throwaway GTK 3 window for testing deskcrew on Linux. It prints what reaches it to stdout."""
import sys
import gi
gi.require_version('Gtk', '3.0')
gi.require_version('Gdk', '3.0')
from gi.repository import Gdk, Gtk

win = Gtk.Window(title='Deskcrew Form')
win.set_default_size(420, 460)
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

button = Gtk.Button(label='Submit')
button.connect('clicked', submit)
grid.attach(button, 1, 3, 1, 1)
win.connect('destroy', Gtk.main_quit)
win.show_all()
print('ready', flush=True)
Gtk.main()
