#!/usr/bin/env python3
"""Çekirdekteki sürücülerin sürüm özetleri (kaynak dosyasının SHA-256'sı).

Sistem güncellemesi bildirimine `driver <ad> <özet>` satırları olarak, kurulu sisteme
/etc/banna/drivers olarak yazılır; hatch değişen sürücüleri bununla listeler.

Kullanım: driver_versions.py [çıktı]
"""
import hashlib
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent


def drivers():
    sources = sorted((ROOT / "src/drivers").rglob("*.rs")) + sorted((ROOT / "src/storage").rglob("*.rs"))
    sources += sorted((ROOT / "modules").glob("*/src/main.rs"))
    out = {}
    for source in sources:
        if source.name == "mod.rs":
            continue
        name = source.parent.parent.name if source.name == "main.rs" else source.stem
        out[name] = hashlib.sha256(source.read_bytes()).hexdigest()[:16]
    return out


def text():
    return "".join(f"{name} {digest}\n" for name, digest in drivers().items())


if __name__ == "__main__":
    if len(sys.argv) > 1:
        pathlib.Path(sys.argv[1]).write_text(text(), encoding="utf-8")
    else:
        sys.stdout.write(text())
