#!/usr/bin/env python3
"""Emulated USB DualSense (054c:0ce6) for testing without a controller.

The kernel's dummy_hcd and a configfs HID gadget make it a real USB device, so hid-playstation and
InputPlumber treat it like a plugged-in pad. (A uhid device would not do: InputPlumber ignores
virtual hidraw nodes unless they belong to a connected Bluetooth device.)

Run as root; Ctrl+C removes the pad. While it runs, send commands to /run/dualsense-gadget.ctl,
one per line, e.g. echo 'tap cross' | sudo tee /run/dualsense-gadget.ctl:
  press <button> | release <button> | tap <button> [ms] | dpad up|down|left|right|none
  unplug | plug | quit
Buttons: ps cross circle square triangle l1 r1 create options l3 r3 touchpad
"""
import errno
import fcntl
import os
import select
import signal
import struct
import sys
import time

# DS_USB_DESCRIPTOR from src/drivers/dualsense/report_descriptor.rs.
DESC = bytes.fromhex("05010905a1018501093009310932093509330934150026ff007508950681020600ff09209501810205010939150025073500463b016514750495018142650005091901290f150025017501950f81020600ff0921950d81020600ff0922150026ff0075089534810285020923952f9102850509339528b10285080934952fb102850909249513b102850a0925951ab10285200926953fb102852109279504b10285220940953fb10285800928953fb10285810929953fb1028582092a9509b1028583092b953fb1028584092c953fb1028585092d9502b10285a0092e9501b10285e0092f953fb10285f00930953fb10285f10931953fb10285f20932950fb10285f40935953fb10285f509369503b102c0")
GADGET = "/sys/kernel/config/usb_gadget/dualsense-gadget"
CTL = "/run/dualsense-gadget.ctl"

# Feature reports hid-playstation reads when it binds: calibration, pairing info (MAC), firmware.
CAL = struct.pack("<B3h6h2h6h", 0x05, 0, 0, 0, 2048, -2048, 2048, -2048, 2048, -2048,
                  125, 125, 8192, -8192, 8192, -8192, 8192, -8192) + bytes(6)
PAIR = bytes([0x09, 0xF2, 0x1A, 0xBC, 0x4B, 0x2F, 0x02, 0x08, 0x25, 0x00, 0x1E, 0x00,
              0xEE, 0x74, 0xD0, 0xBC, 0, 0, 0, 0])
FW = bytes.fromhex("204a756e203139203230323331343a34373a3334030044000802000136000001c1c8"
                   "0000000000000000000054010000140000000b0001000600000000000000")
# GADGET_HID_WRITE_GET_REPORT from <linux/usb/g_hid.h>: _IOW('g', 0x42, struct usb_hidg_report).
WRITE_GET_REPORT = 0x40000000 | (72 << 16) | (ord("g") << 8) | 0x42

BUTTONS = {  # byte and bit in the 64-byte USB input report
    "square": (8, 4), "cross": (8, 5), "circle": (8, 6), "triangle": (8, 7),
    "l1": (9, 0), "r1": (9, 1), "create": (9, 4), "options": (9, 5), "l3": (9, 6), "r3": (9, 7),
    "ps": (10, 0), "touchpad": (10, 1),
}
DPAD = {"up": 0, "right": 2, "down": 4, "left": 6, "none": 8}


def log(msg):
    print(f"{time.strftime('%H:%M:%S')} dualsense-gadget: {msg}", flush=True)


def write(path, data):
    with open(path, "wb" if isinstance(data, bytes) else "w") as f:
        f.write(data)


def create_gadget():
    for module in ("libcomposite", "usb_f_hid", "dummy_hcd"):
        if os.system(f"modprobe {module}") != 0:
            sys.exit(f"modprobe {module} failed")
    os.makedirs(f"{GADGET}/strings/0x409")
    write(f"{GADGET}/idVendor", "0x054c")
    write(f"{GADGET}/idProduct", "0x0ce6")
    write(f"{GADGET}/bcdDevice", "0x0100")
    write(f"{GADGET}/bcdUSB", "0x0200")
    write(f"{GADGET}/strings/0x409/manufacturer", "Sony Interactive Entertainment")
    write(f"{GADGET}/strings/0x409/product", "DualSense Wireless Controller")
    hid = f"{GADGET}/functions/hid.usb0"
    os.makedirs(hid)
    write(f"{hid}/protocol", "0")
    write(f"{hid}/subclass", "0")
    write(f"{hid}/report_length", "64")
    write(f"{hid}/report_desc", DESC)
    os.makedirs(f"{GADGET}/configs/c.1")
    os.symlink(hid, f"{GADGET}/configs/c.1/hid.usb0")


def remove_gadget():
    if not os.path.isdir(GADGET):
        return
    try:
        write(f"{GADGET}/UDC", "\n")
    except OSError:
        pass
    if os.path.islink(f"{GADGET}/configs/c.1/hid.usb0"):
        os.unlink(f"{GADGET}/configs/c.1/hid.usb0")
    for d in ("configs/c.1", "functions/hid.usb0", "strings/0x409", ""):
        os.rmdir(f"{GADGET}/{d}".rstrip("/"))


def hidg_node():
    major, minor = map(int, open(f"{GADGET}/functions/hid.usb0/dev").read().split(":"))
    for name in os.listdir("/dev"):
        if name.startswith("hidg"):
            rdev = os.stat(f"/dev/{name}").st_rdev
            if (os.major(rdev), os.minor(rdev)) == (major, minor):
                return f"/dev/{name}"
    return None


def plug():
    udc = sorted(os.listdir("/sys/class/udc"))[0]
    write(f"{GADGET}/UDC", udc)
    deadline = time.time() + 2
    node = None
    while not node and time.time() < deadline:
        node = hidg_node()
        time.sleep(0.002)
    fd = os.open(node, os.O_RDWR | os.O_NONBLOCK)
    for report in (CAL, PAIR, FW):
        reply = struct.pack("<BBH", report[0], 0, len(report)) + report.ljust(64, b"\0") + bytes(4)
        fcntl.ioctl(fd, WRITE_GET_REPORT, reply)
    log(f"plugged in on {udc}")
    return fd


def unplug(fd):
    os.close(fd)
    write(f"{GADGET}/UDC", "\n")
    log("unplugged")


def ignore_gone(call, *args):
    try:
        return call(*args)
    except OSError as e:
        if e.errno not in (errno.EAGAIN, errno.ESHUTDOWN, errno.EIO):
            raise
    return None


def main():
    if os.path.exists(CTL):
        os.unlink(CTL)
    os.mkfifo(CTL, 0o600)
    ctl = os.open(CTL, os.O_RDWR | os.O_NONBLOCK)
    create_gadget()
    fd = plug()
    report = bytearray(64)
    report[0] = 0x01  # USB input report
    report[1:5] = b"\x80\x80\x80\x80"  # sticks centred
    report[8] = DPAD["none"]
    report[33] = report[37] = 0x80  # no touch
    report[53] = 0x08  # battery
    pending = []
    buf = b""
    seq = 0
    try:
        while True:
            if select.select([ctl], [], [], 0.004)[0]:
                buf += os.read(ctl, 4096)
            while b"\n" in buf:
                line, buf = buf.split(b"\n", 1)
                cmd = line.decode().split()
                if not cmd:
                    continue
                log(" ".join(cmd))
                if cmd[0] in ("press", "release", "tap") and len(cmd) > 1 and cmd[1] in BUTTONS:
                    byte, bit = BUTTONS[cmd[1]]
                    if cmd[0] == "release":
                        report[byte] &= ~(1 << bit) & 0xFF
                    else:
                        report[byte] |= 1 << bit
                    if cmd[0] == "tap":
                        ms = int(cmd[2]) if len(cmd) > 2 else 120
                        pending.append((time.time() + ms / 1000, byte, bit))
                elif cmd[0] == "dpad" and len(cmd) > 1 and cmd[1] in DPAD:
                    report[8] = (report[8] & 0xF0) | DPAD[cmd[1]]
                elif cmd[0] == "unplug" and fd is not None:
                    unplug(fd)
                    fd = None
                elif cmd[0] == "plug" and fd is None:
                    fd = plug()
                elif cmd[0] == "quit":
                    return
            now = time.time()
            for item in [p for p in pending if p[0] <= now]:
                report[item[1]] &= ~(1 << item[2]) & 0xFF
                pending.remove(item)
            if fd is None:
                continue
            seq = (seq + 1) & 0xFF
            report[7] = seq
            report[28:32] = struct.pack("<I", int(now * 1e6) & 0xFFFFFFFF)
            ignore_gone(os.write, fd, bytes(report))
            while ignore_gone(os.read, fd, 64):  # output reports (LEDs, rumble) are dropped
                pass
    finally:
        if fd is not None:
            ignore_gone(unplug, fd)
        remove_gadget()
        os.unlink(CTL)
        log("removed")


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    try:
        main()
    except KeyboardInterrupt:
        pass
