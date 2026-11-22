#!/usr/bin/env python3
"""Place an .sbat section objcopy appended at virtual address 0.

objcopy adds the section at the front of a PE and leaves its virtual
address at 0, which overlaps the headers once the image is loaded. Shim
finds the section by name; the firmware still has to load it without
covering the rest of the image, so it goes on the next page after the
image's own sections and SizeOfImage grows to cover it.
"""

import pathlib
import struct
import sys


def align_up(value, alignment):
    return (value + alignment - 1) & ~(alignment - 1)


def main(path):
    file = pathlib.Path(path)
    image = bytearray(file.read_bytes())
    pe = struct.unpack_from("<I", image, 0x3C)[0]
    if image[pe : pe + 4] != b"PE\x00\x00":
        raise SystemExit(f"{path} is not a PE")
    magic = struct.unpack_from("<H", image, pe + 24)[0]
    if magic != 0x20B:
        raise SystemExit(f"{path} is not PE32+ ({magic:#x})")
    optional = pe + 24
    section_align = struct.unpack_from("<I", image, optional + 32)[0]
    number = struct.unpack_from("<H", image, pe + 6)[0]
    optional_size = struct.unpack_from("<H", image, pe + 20)[0]
    offset = pe + 24 + optional_size
    sbat = None
    end = 0
    for _ in range(number):
        name = bytes(image[offset : offset + 8]).split(b"\x00", 1)[0]
        virtual_size, virtual_address = struct.unpack_from("<II", image, offset + 8)
        if name == b".sbat":
            sbat = offset
        else:
            end = max(end, virtual_address + virtual_size)
        offset += 40
    if sbat is None:
        raise SystemExit(f"{path} has no .sbat section")
    virtual_address = align_up(end, section_align)
    virtual_size = struct.unpack_from("<I", image, sbat + 8)[0]
    struct.pack_into("<I", image, sbat + 12, virtual_address)
    struct.pack_into("<I", image, optional + 56, align_up(virtual_address + virtual_size, section_align))
    # The previous checksum described the unsigned image. EFI accepts zero.
    struct.pack_into("<I", image, optional + 64, 0)
    file.write_bytes(image)


if __name__ == "__main__":
    main(sys.argv[1])
