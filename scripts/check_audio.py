#!/usr/bin/env python3
"""QEMU'nun wav ses arka ucunun kaydında ses var mı ve frekansı aralıkta mı denetler.

Kullanım: check_audio.py <kayıt.wav> <en düşük Hz> <en yüksek Hz>
(QEMU başlıktaki boyutları doldurmayabildiği için veri 44. bayttan okunur.)
"""
import struct
import sys

path, low, high = sys.argv[1], float(sys.argv[2]), float(sys.argv[3])
data = open(path, "rb").read()
channels = struct.unpack("<H", data[22:24])[0]
rate = struct.unpack("<I", data[24:28])[0]
raw = data[44:]
samples = struct.unpack("<%dh" % (len(raw) // 2), raw[: len(raw) // 2 * 2])[::channels]
loud = [i for i, v in enumerate(samples) if abs(v) > 1000]
if not loud:
    print("audio: no sound recorded")
    sys.exit(1)
# Bipler arasındaki sessizlik ortalamayı düşürmesin: yalnızca sesli 10 ms pencereler.
window = rate // 100
crossings = 0
voiced = 0
for start in range(loud[0], loud[-1], window):
    chunk = samples[start : start + window]
    if max(abs(v) for v in chunk) <= 1000:
        continue
    voiced += len(chunk)
    crossings += sum(1 for i in range(1, len(chunk)) if (chunk[i - 1] < 0) != (chunk[i] < 0))
duration = voiced / rate
frequency = crossings / 2 / duration
print(f"audio: {duration:.2f} s of sound, ~{frequency:.0f} Hz")
sys.exit(0 if low <= frequency <= high else 1)
