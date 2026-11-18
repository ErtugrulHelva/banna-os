#!/usr/bin/env python3
"""banna-update için imzalı sistem güncellemesi üretir.

Girdi: build/rootfs (programlar, uygulamalar, /usr/share), sadeleştirilmiş çekirdek ve
UEFI yükleyicisi. Çıktı: <çıktı>/manifest, <çıktı>/manifest.sig ve <çıktı>/files/<yol>.
Biçim user/src/bin/banna-update.rs ile aynıdır.

Kullanım: build_system.py [çıktı] [--version SÜRÜM] [--file /yol=kaynak ...]
"""
import argparse
import datetime
import hashlib
import pathlib
import shutil
import subprocess
import sys

from driver_versions import drivers

ROOT = pathlib.Path(__file__).resolve().parent.parent
BUILD = ROOT / "build"
ROOTFS = BUILD / "rootfs"
KEY = ROOT / "keys/hatch-signing.pem"
EXTRA = {
    "/boot/boot/banna_os": BUILD / "banna_os.stripped",
    "/boot/EFI/BOOT/BOOTX64.EFI": ROOT / "loader/target/x86_64-unknown-uefi/release/banna-loader.efi",
}


def collect(overrides):
    files = {}
    for top in ("bin", "apps", "usr/share", "lib/firmware", "lib/modules"):
        for source in sorted((ROOTFS / top).rglob("*")):
            if source.is_file() and source.name != ".DS_Store":
                files["/" + source.relative_to(ROOTFS).as_posix()] = source
    for path, source in EXTRA.items():
        if not source.is_file():
            sys.exit(f"{source} yok (önce: make disk)")
        files[path] = source
    for item in overrides:
        path, _, source = item.partition("=")
        files[path] = pathlib.Path(source)
    return files


def release_notes():
    """CHANGELOG.md'nin en üstteki bölümü: (başlık, maddeler)."""
    path = ROOT / "CHANGELOG.md"
    if not path.is_file():
        return "", []
    title, items = "", []
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.startswith("## "):
            if title:
                break
            title = line[3:].strip()
        elif title and line.startswith("- "):
            items.append(" ".join(line[2:].split()))
    return title, items


def build(out, version, overrides):
    if not KEY.is_file():
        sys.exit(f"imza anahtarı yok: {KEY}")
    version = version or (ROOTFS / "etc/banna/version").read_text().strip()
    if (out / "files").exists():
        shutil.rmtree(out / "files")
    title, items = release_notes()
    # `notes` tek satırlık özettir (eski istemciler yalnızca onu okur); `note` maddeleri ve
    # `date` Banna Store'un sistem güncellemesi sayfasındadır.
    lines = [f"banna-system 1\nversion {version}\nnotes {title or f'Banna OS {version}'}\n"]
    lines.append(f"date {datetime.date.today().isoformat()}\n")
    lines += [f"note {item}\n" for item in items]
    lines += [f"driver {name} {digest}\n" for name, digest in drivers().items()]
    for path, source in collect(overrides).items():
        data = source.read_bytes()
        target = out / "files" / path.lstrip("/")
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
        lines.append(f"file {hashlib.sha256(data).hexdigest()} {len(data)} {path}\n")
    (out / "manifest").write_text("".join(lines), encoding="utf-8")
    subprocess.run(
        ["openssl", "dgst", "-sha256", "-sign", str(KEY), "-out", str(out / "manifest.sig"), str(out / "manifest")],
        check=True,
    )
    files = sum(1 for line in lines if line.startswith("file "))
    print(f"sistem güncellemesi: {out} (sürüm {version}, {files} dosya)")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("out", nargs="?", default=BUILD / "repo/system", type=pathlib.Path)
    parser.add_argument("--version")
    parser.add_argument("--file", action="append", default=[])
    args = parser.parse_args()
    build(args.out, args.version, args.file)
