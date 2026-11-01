# A USB camera for checks.peripherals (peripherals.nix), made by the kernel's
# raw gadget on its dummy controller, as QEMU has no camera to plug in. The
# kernel's own webcam gadget streams over isochronous endpoints, which the
# dummy controller has none of, and FunctionFS takes no video class
# descriptors, so this is the whole device: its descriptors, its answers to
# the probe and commit uvcvideo makes, and a bulk endpoint sending 160x120
# YUYV frames of one colour, ten a second, while uvcvideo reads them.
#
#     python3 uvc-camera.py
#
# It runs until it is killed, which pulls the camera out.

import ctypes
import os
import struct
import threading

libc = ctypes.CDLL(None, use_errno=True)

# linux/usb/raw_gadget.h, for this 64-bit layout.
INIT = 0x41015500
RUN = 0x5501
EVENT_FETCH = 0x80085502
EP0_WRITE = 0x40085503
EP0_READ = 0xC0085504
EP_ENABLE = 0x40095505
EP_DISABLE = 0x40045506
EP_WRITE = 0x40085507
CONFIGURE = 0x5509
VBUS_DRAW = 0x4004550A
EP0_STALL = 0x550C
EVENT_CONTROL = 2
EVENT_RESET = 5
EVENT_DISCONNECT = 6
SPEED_HIGH = 3

WIDTH, HEIGHT, INTERVAL = 160, 120, 1_000_000  # 100 ns units: 10 a second
FRAME = WIDTH * HEIGHT * 2
PAYLOAD = FRAME + 2
# Red: Y, U, Y, V for each two pixels.
PIXELS = bytes([81, 90, 81, 240]) * (FRAME // 4)


def ioctl(fd, request, arg=0):
    r = libc.ioctl(fd, ctypes.c_ulong(request), arg)
    if r < 0:
        e = ctypes.get_errno()
        raise OSError(e, os.strerror(e))
    return r


def desc(kind, *fields):
    body = b"".join(fields)
    return bytes([len(body) + 2, kind]) + body


def cs(subtype, *fields):
    return desc(0x24, bytes([subtype]), *fields)


u8 = lambda v: struct.pack("<B", v)
u16 = lambda v: struct.pack("<H", v)
u32 = lambda v: struct.pack("<I", v)

DEVICE = desc(
    0x01, u16(0x0200), bytes([0xEF, 0x02, 0x01, 64]), u16(0x1D6B), u16(0x0102),
    u16(0x0100), bytes([1, 2, 0, 1]),
)
QUALIFIER = desc(0x06, u16(0x0200), bytes([0xEF, 0x02, 0x01, 64, 1, 0]))
ENDPOINT = desc(0x05, bytes([0x81, 0x02]), u16(512), u8(0))

vc_units = (
    # The camera terminal and the streaming terminal it feeds.
    cs(0x02, u8(1), u16(0x0201), u8(0), u8(0), u16(0), u16(0), u16(0), u8(3), bytes(3))
    + cs(0x03, u8(2), u16(0x0101), u8(0), u8(1), u8(0))
)
vc_header = cs(0x01, u16(0x0100), u16(13 + len(vc_units)), u32(48_000_000), u8(1), u8(1))
YUY2 = b"YUY2" + bytes([0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71])
vs_formats = cs(0x04, u8(1), u8(1), YUY2, u8(16), u8(1), bytes(4)) + cs(
    0x05, u8(1), u8(0), u16(WIDTH), u16(HEIGHT), u32(FRAME * 80), u32(FRAME * 80),
    u32(FRAME), u32(INTERVAL), u8(1), u32(INTERVAL),
)
vs_header = cs(
    0x01, u8(1), u16(14 + len(vs_formats)), u8(0x81), u8(0), u8(2), bytes(3), u8(1), u8(0)
)
interfaces = (
    desc(0x0B, bytes([0, 2, 0x0E, 0x03, 0x00, 2]))
    + desc(0x04, bytes([0, 0, 0, 0x0E, 0x01, 0x00, 2]))
    + vc_header + vc_units
    + desc(0x04, bytes([1, 0, 1, 0x0E, 0x02, 0x00, 0]))
    + vs_header + vs_formats
    + ENDPOINT
)
CONFIG = desc(0x02, u16(9 + len(interfaces)), bytes([2, 1, 0, 0x80, 250])) + interfaces
STRINGS = {
    0: desc(0x03, u16(0x0409)),
    1: desc(0x03, "LosOS".encode("utf-16-le")),
    2: desc(0x03, "LosOS Test Camera".encode("utf-16-le")),
}
# The one setting probe and commit agree on.
PROBE = struct.pack("<HBBIHHHHHII", 1, 1, 1, INTERVAL, 0, 0, 0, 0, 0, FRAME, PAYLOAD)

fd = os.open("/dev/raw-gadget", os.O_RDWR)
streaming = threading.Event()
ep = {"handle": None}


def stream():
    fid = 0
    while True:
        streaming.wait()
        payload = bytes([2, 0x80 | 0x02 | fid]) + PIXELS
        io = struct.pack("<HHI", ep["handle"] or 0, 0, len(payload)) + payload
        try:
            ioctl(fd, EP_WRITE, ctypes.create_string_buffer(io, len(io)))
            fid ^= 1
        except OSError:
            # A reset, or the endpoint gone with the configuration.
            streaming.clear()


def answer(data):
    io = struct.pack("<HHI", 0, 0, len(data)) + data
    ioctl(fd, EP0_WRITE, ctypes.create_string_buffer(io, len(io)))


def ack(length=0):
    io = ctypes.create_string_buffer(struct.pack("<HHI", 0, 0, length) + bytes(length))
    ioctl(fd, EP0_READ, io)
    return io.raw[8 : 8 + length]


def control(kind, request, value, index, length):
    if kind == 0x80 and request == 0x06:
        what, n = value >> 8, value & 0xFF
        data = {1: DEVICE, 2: CONFIG, 6: QUALIFIER}.get(what) if what != 3 else STRINGS.get(n)
        if data is None:
            return ioctl(fd, EP0_STALL)
        return answer(data[:length])
    if kind == 0x00 and request == 0x09:
        if ep["handle"] is not None:
            streaming.clear()
            try:
                ioctl(fd, EP_DISABLE, ep["handle"])
            except OSError:
                pass
            ep["handle"] = None
        if value == 1:
            ep["handle"] = ioctl(fd, EP_ENABLE, ctypes.create_string_buffer(ENDPOINT + bytes(2), 9))
            ioctl(fd, VBUS_DRAW, 250)
            ioctl(fd, CONFIGURE)
        return ack()
    if kind in (0x00, 0x01, 0x02) and request in (0x01, 0x03, 0x0B):
        return ack()
    if kind == 0x80 and request == 0x08:
        return answer(b"\x01"[:length])
    if kind in (0x80, 0x81, 0x82) and request == 0x00:
        return answer(b"\x00\x00"[:length])
    if kind == 0x81 and request == 0x0A:
        return answer(b"\x00"[:length])
    # The streaming interface's probe and commit; nothing else of the
    # video class is answered, which uvcvideo takes as no such control.
    if index & 0xFF == 1 and value >> 8 in (1, 2):
        if kind == 0xA1:
            data = {0x85: u16(len(PROBE)), 0x86: b"\x03"}.get(request, PROBE)
            return answer(data[:length])
        if kind == 0x21 and request == 0x01:
            ack(length)
            if value >> 8 == 2:
                streaming.set()
            return
    ioctl(fd, EP0_STALL)


init = ctypes.create_string_buffer(257)
init[0:9] = b"dummy_udc"
init[128:139] = b"dummy_udc.0"
init[256] = SPEED_HIGH
ioctl(fd, INIT, init)
ioctl(fd, RUN)
threading.Thread(target=stream, daemon=True).start()
print("a camera, LosOS Test Camera", flush=True)
while True:
    event = ctypes.create_string_buffer(struct.pack("<II", 0, 8) + bytes(8))
    ioctl(fd, EVENT_FETCH, event)
    kind, _ = struct.unpack("<II", event.raw[:8])
    if kind in (EVENT_RESET, EVENT_DISCONNECT):
        streaming.clear()
    elif kind == EVENT_CONTROL:
        try:
            control(*struct.unpack("<BBHHH", event.raw[8:16]))
        except OSError as e:
            print("control", e, flush=True)
