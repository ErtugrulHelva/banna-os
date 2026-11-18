#!/usr/bin/env python3
"""QEMU'da batarya sürücüsünü sınamak için sahte bir batarya içeren SSDT üretir.

Kullanım: gen_test_battery_ssdt.py <çıktı.aml>
QEMU: -acpitable file=<çıktı.aml>

Beklenen sonuç: BAT0 %75, deşarj, 3 saat kaldı; AC adaptör çevrimdışı.
"""

import struct
import sys


def pkg(body: bytes) -> bytes:
    for extra in range(1, 5):
        total = len(body) + extra
        if extra == 1 and total < 0x40:
            return bytes([total]) + body
        if extra > 1 and total < (1 << (4 + 8 * (extra - 1))):
            lead = ((extra - 1) << 6) | (total & 0x0F)
            rest = [(total >> (4 + 8 * i)) & 0xFF for i in range(extra - 1)]
            return bytes([lead] + rest) + body
    raise ValueError("package too large")


def name(text: str) -> bytes:
    return text.ljust(4, "_").encode()


def path(text: str) -> bytes:
    absolute = text.startswith("\\")
    segments = [s for s in text.lstrip("\\").split(".") if s]
    prefix = b"\\" if absolute else b""
    if len(segments) == 1:
        return prefix + name(segments[0])
    if len(segments) == 2:
        return prefix + b"\x2e" + b"".join(name(s) for s in segments)
    return prefix + b"\x2f" + bytes([len(segments)]) + b"".join(name(s) for s in segments)


def integer(value: int) -> bytes:
    if value == 0:
        return b"\x00"
    if value == 1:
        return b"\x01"
    if value <= 0xFF:
        return b"\x0a" + bytes([value])
    if value <= 0xFFFF:
        return b"\x0b" + struct.pack("<H", value)
    if value <= 0xFFFFFFFF:
        return b"\x0c" + struct.pack("<I", value)
    return b"\x0e" + struct.pack("<Q", value)


def string(text: str) -> bytes:
    return b"\x0d" + text.encode() + b"\x00"


def eisa_id(text: str) -> bytes:
    vendor = ((ord(text[0]) - 0x40) << 10) | ((ord(text[1]) - 0x40) << 5) | (ord(text[2]) - 0x40)
    product = int(text[3:], 16)
    return b"\x0c" + bytes([vendor >> 8, vendor & 0xFF, product >> 8, product & 0xFF])


def package(*items: bytes) -> bytes:
    return b"\x12" + pkg(bytes([len(items)]) + b"".join(items))


def name_op(n: str, value: bytes) -> bytes:
    return b"\x08" + name(n) + value


def method(n: str, args: int, body: bytes) -> bytes:
    return b"\x14" + pkg(name(n) + bytes([args]) + body)


def device(n: str, body: bytes) -> bytes:
    return b"\x5b\x82" + pkg(name(n) + body)


def scope(p: str, body: bytes) -> bytes:
    return b"\x10" + pkg(path(p) + body)


def ret(value: bytes) -> bytes:
    return b"\xa4" + value


LOCAL0, LOCAL1 = b"\x60", b"\x61"


def store(value: bytes, target: bytes) -> bytes:
    return b"\x70" + value + target


def while_(predicate: bytes, body: bytes) -> bytes:
    return b"\xa2" + pkg(predicate + body)


def lless(a: bytes, b: bytes) -> bytes:
    return b"\x95" + a + b


def add(a: bytes, b: bytes, target: bytes) -> bytes:
    return b"\x72" + a + b + target


def increment(target: bytes) -> bytes:
    return b"\x75" + target


def index(source: bytes, i: bytes) -> bytes:
    return b"\x88" + source + i + b"\x00"


battery = device(
    "BAT0",
    name_op("_HID", eisa_id("PNP0C0A"))
    + name_op("_UID", integer(1))
    + name_op(
        "PBIF",
        package(
            integer(1),
            integer(5000),
            integer(4800),
            integer(1),
            integer(11100),
            integer(480),
            integer(240),
            integer(10),
            integer(10),
            string("Banna Cell"),
            string("0042"),
            string("LION"),
            string("BannaPower"),
        ),
    )
    + name_op("PBST", package(integer(1), integer(1200), integer(0), integer(11400)))
    + method("_STA", 0, ret(integer(0x1F)))
    + method("_BIF", 0, ret(name("PBIF")))
    + method(
        "_BST",
        0,
        store(integer(0), LOCAL0)
        + store(integer(0), LOCAL1)
        + while_(lless(LOCAL1, integer(75)), add(LOCAL0, integer(48), LOCAL0) + increment(LOCAL1))
        + store(LOCAL0, index(name("PBST"), integer(2)))
        + ret(name("PBST")),
    ),
)

adapter = device(
    "ADP0",
    name_op("_HID", string("ACPI0003")) + method("_PSR", 0, ret(integer(0))),
)

aml = scope("\\_SB", battery + adapter)

header = bytearray(
    b"SSDT"
    + struct.pack("<I", 36 + len(aml))
    + bytes([2, 0])
    + b"BANNA "
    + b"TESTBAT "
    + struct.pack("<I", 1)
    + b"BNNA"
    + struct.pack("<I", 1)
)
table = header + aml
table[9] = (-sum(table)) & 0xFF

with open(sys.argv[1], "wb") as out:
    out.write(table)
