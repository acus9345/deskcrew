"""A throwaway GTK 3 window for testing deskcrew on Linux. It prints what reaches it to stdout."""
import sys
import gi
gi.require_version('Gtk', '3.0')
from gi.repository import Gtk

win = Gtk.Window(title='Deskcrew Form')
win.set_default_size(420, 360)
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

button = Gtk.Button(label='Submit')
button.connect('clicked', submit)
grid.attach(button, 1, 3, 1, 1)
win.connect('destroy', Gtk.main_quit)
win.show_all()
print('ready', flush=True)
Gtk.main()
