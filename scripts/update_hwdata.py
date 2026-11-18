#!/usr/bin/env python3
"""PCI ve USB kimlik listelerini indirir, sadeleştirip assets/hwdata altına yazar.

`hatch diagnose` aygıt kimliklerini (10ec:8168) bu listelerle satıcı ve model adına çevirir.
Baştaki açıklama (lisans) bloğu korunur; alt sistem satırları ve sınıf tabloları atılır.
"""

import pathlib
import urllib.request

OUT = pathlib.Path(__file__).resolve().parent.parent / "assets" / "hwdata"
SOURCES = {
    "pci.ids": "https://pci-ids.ucw.cz/v2.2/pci.ids",
    "usb.ids": "http://www.linux-usb.org/usb.ids",
}


def trim(text):
    lines = []
    header = True
    for line in text.splitlines():
        if line.startswith("# List of known device classes"):
            break
        if line.startswith("#"):
            if header:
                lines.append(line)
            continue
        if not line.strip():
            continue
        header = False
        if not line.startswith("\t\t"):
            lines.append(line)
    return "\n".join(lines) + "\n"


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    for name, url in SOURCES.items():
        with urllib.request.urlopen(url, timeout=60) as response:
            text = response.read().decode("utf-8", "replace")
        (OUT / name).write_text(trim(text), encoding="utf-8")
        print(f"{name}: {(OUT / name).stat().st_size} bayt")


if __name__ == "__main__":
    main()
