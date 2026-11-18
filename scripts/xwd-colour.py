"""Count the pixels of one colour in an X window dump, or check for a typing caret.

    xwd -root -silent | python3 scripts/xwd-colour.py 217 119 87
    xwd -root -silent | python3 scripts/xwd-colour.py --caret 217 119 87

Prints the pixel count, or with --caret the height in rows of a narrow vertical
caret bar sitting beneath a wide name tag (0 if none is found).
Reads the XWD format directly, so it needs nothing beyond Python.
"""
import struct
import sys

args = sys.argv[1:]
caret_mode = False
if args and args[0] == '--caret':
    caret_mode = True
    args = args[1:]
want = tuple(int(n) for n in args[:3])
data = sys.stdin.buffer.read()
if len(data) < 100:
    sys.exit('incomplete XWD header')
fields = struct.unpack('>25I', data[:100])
header_size, depth, width, height = fields[0], fields[3], fields[4], fields[5]
byte_order, bits_per_pixel, bytes_per_line = fields[7], fields[11], fields[12]
red_mask, green_mask, blue_mask, colours = fields[14], fields[15], fields[16], fields[19]
if bits_per_pixel not in (24, 32):
    sys.exit(f'only 24 or 32 bits a pixel are read here, not {bits_per_pixel}')
if not (red_mask and green_mask and blue_mask):
    sys.exit('XWD colour masks are zero')
size = bits_per_pixel // 8
pixels = data[header_size + colours * 12:]
order = '<' if byte_order == 0 else '>'


def channel(value, mask):
    shift = (mask & -mask).bit_length() - 1
    return (value & mask) >> shift


rows = {}
for y in range(height):
    row = pixels[y * bytes_per_line:y * bytes_per_line + width * size]
    xs = []
    for x, at in enumerate(range(0, len(row) - size + 1, size)):
        value = int.from_bytes(row[at:at + size], 'little' if order == '<' else 'big')
        if (channel(value, red_mask), channel(value, green_mask), channel(value, blue_mask)) == want:
            xs.append(x)
    if xs:
        rows[y] = xs

if not caret_mode:
    print(sum(len(xs) for xs in rows.values()))
    sys.exit(0)

tag_rows = [y for y, xs in rows.items() if len(xs) >= 11]
if not tag_rows:
    print(0)
    sys.exit(0)

tag_bottom = max(tag_rows)
# Include rounded bottom rows of the tag itself (e.g. when no font is installed and the tag is 13px wide).
while tag_bottom + 1 in rows and len(rows[tag_bottom + 1]) >= 5:
    tag_bottom += 1
    tag_rows.append(tag_bottom)
tag_left = min(min(rows[y]) for y in tag_rows)
tag_right = max(max(rows[y]) for y in tag_rows)
below = [(y, xs) for y, xs in sorted(rows.items()) if y > tag_bottom]
if not below or any(len(xs) > 4 or max(xs) - min(xs) > 3 for _, xs in below):
    print(0)
    sys.exit(0)

# Expect a transparent gap of at least 2 rows between the tag and the vertical caret bar.
if below[0][0] - tag_bottom < 3:
    print(0)
    sys.exit(0)

bar_xs = [x for _, xs in below for x in xs]
if max(bar_xs) - min(bar_xs) > 3 or min(bar_xs) < tag_left - 2 or max(bar_xs) > tag_right + 2:
    print(0)
    sys.exit(0)

bar_rows = [y for y, xs in below if 2 <= len(xs) <= 4 and max(xs) - min(xs) + 1 == len(xs)]
print(len(bar_rows))
