#!/usr/bin/env python3
"""Örnek duvar kağıdı (24 bit BMP): yıldızlı gökyüzü, kuzey ışıkları, dağlar ve göl.

Kullanım: gen_wallpaper.py <çıktı.bmp> [genişlik] [yükseklik]
"""
import math
import random
import struct
import sys

path = sys.argv[1]
W = int(sys.argv[2]) if len(sys.argv) > 2 else 1600
H = int(sys.argv[3]) if len(sys.argv) > 3 else 900
random.seed(7)


def mix(a, b, t):
    t = max(0.0, min(1.0, t))
    return tuple(a[i] + (b[i] - a[i]) * t for i in range(3))


horizon = int(H * 0.68)
ridges = []
for k, (base, amp, color) in enumerate(
    [(0.50, 0.10, (52, 48, 92)), (0.58, 0.08, (34, 32, 66)), (0.64, 0.05, (18, 18, 40))]
):
    phase = [random.random() * 6.28 for _ in range(4)]
    heights = []
    for x in range(W):
        u = x / W
        h = (
            math.sin(u * (3 + k) * 3.1 + phase[0]) * 0.55
            + math.sin(u * (7 + 2 * k) * 3.1 + phase[1]) * 0.3
            + math.sin(u * (17 + k) * 3.1 + phase[2]) * 0.1
            + abs(math.sin(u * (31 + k) * 3.1 + phase[3])) * 0.05
        )
        heights.append(int(H * (base - amp * h)))
    ridges.append((heights, color))

stars = {(random.randrange(W), random.randrange(int(H * 0.55))) for _ in range(W * H // 1800)}

rows = []
for y in range(H):
    row = bytearray()
    for x in range(W):
        ry = y if y < horizon else 2 * horizon - y
        t = ry / horizon
        c = mix((8, 12, 34), (40, 34, 86), t)
        # Kuzey ışıkları: yatay dalgalanan yeşil-mor şerit.
        band = H * (0.24 + 0.06 * math.sin(x / W * 9.0) + 0.03 * math.sin(x / W * 23.0 + 1.3))
        d = (ry - band) / (H * 0.09)
        glow = math.exp(-d * d) * (0.55 + 0.45 * math.sin(x / W * 40.0) ** 2)
        c = mix(c, (60, 230, 160), glow * 0.75)
        c = mix(c, (150, 90, 230), math.exp(-((ry - band + H * 0.08) / (H * 0.07)) ** 2) * 0.35)
        if (x, ry) in stars:
            c = mix(c, (255, 255, 255), 0.8)
        for heights, color in ridges:
            if ry >= heights[x]:
                c = color
        if y >= horizon:
            ripple = 0.08 * math.sin(y * 0.9 + math.sin(x * 0.02) * 2)
            c = mix(c, (10, 14, 30), 0.5 + ripple)
        row += bytes((int(c[2]), int(c[1]), int(c[0])))
    row += b"\0" * ((4 - len(row) % 4) % 4)
    rows.append(bytes(row))

data = b"".join(reversed(rows))
header = b"BM" + struct.pack("<IHHI", 54 + len(data), 0, 0, 54)
info = struct.pack("<IiiHHIIiiII", 40, W, H, 1, 24, 0, len(data), 2835, 2835, 0, 0)
with open(path, "wb") as f:
    f.write(header + info + data)
print(f"wallpaper: {path} ({W}x{H})")
