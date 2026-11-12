"""Count the pixels of one colour in an X window dump, to check the drawn cursors show.

    xwd -root -silent | python3 scripts/xwd-colour.py 208 111 89

Prints the count. Reads the XWD format directly, so it needs nothing beyond Python.
"""
import struct
import sys

want = tuple(int(n) for n in sys.argv[1:4])
data = sys.stdin.buffer.read()
fields = struct.unpack('>25I', data[:100])
header_size, depth, width, height = fields[0], fields[3], fields[4], fields[5]
byte_order, bits_per_pixel, bytes_per_line = fields[7], fields[11], fields[12]
red_mask, green_mask, blue_mask, colours = fields[14], fields[15], fields[16], fields[19]
if bits_per_pixel not in (24, 32):
    sys.exit(f'only 24 or 32 bits a pixel are read here, not {bits_per_pixel}')
size = bits_per_pixel // 8
pixels = data[header_size + colours * 12:]
order = '<' if byte_order == 0 else '>'


def channel(value, mask):
    shift = (mask & -mask).bit_length() - 1
    return (value & mask) >> shift


count = 0
for y in range(height):
    row = pixels[y * bytes_per_line:y * bytes_per_line + width * size]
    for at in range(0, len(row) - size + 1, size):
        value = int.from_bytes(row[at:at + size], 'little' if order == '<' else 'big')
        if (channel(value, red_mask), channel(value, green_mask), channel(value, blue_mask)) == want:
            count += 1
print(count)
