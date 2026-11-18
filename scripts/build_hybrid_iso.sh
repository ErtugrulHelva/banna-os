#!/bin/bash
# Hibrit Banna OS imajı: USB'ye ham yazılınca (balenaEtcher, Rufus "DD" kipi, dd) Live USB olur,
# CD/sanal makinede ISO olarak açılır.
#
# İçerik: build/disk.img'deki EFI bölümü (Banna UEFI yükleyicisi + çekirdek, cmdline=live) ve
# ext2 kök bölümü GPT bölümleri olarak eklenir; ISO9660 kısmında eski BIOS'lar için Limine durur.
#
# Kullanım: build_hybrid_iso.sh <disk.img> <çıktı.iso> <çekirdek> <limine-dizini>
set -euo pipefail

DISK="$1"
OUT="$2"
KERNEL="$3"
LIMINE_DIR="$4"
WORK="$(dirname "$OUT")/iso_hybrid"

ESP_TYPE="C12A7328-F81F-11D2-BA4B-00A0C93EC93B"
ROOT_TYPE="4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709"

rm -rf "$WORK"
mkdir -p "$WORK/tree/boot/limine"

# GPT'den EFI ve kök bölümlerini ayrı dosyalara çıkar.
python3 - "$DISK" "$WORK" "$ESP_TYPE" "$ROOT_TYPE" <<'EOF'
import struct, sys, uuid
disk, work, esp_type, root_type = sys.argv[1:]
with open(disk, "rb") as d:
    d.seek(512)
    header = d.read(92)
    if header[:8] != b"EFI PART":
        sys.exit(f"{disk}: GPT değil")
    lba, count, size = struct.unpack("<QII", header[72:88])
    d.seek(lba * 512)
    table = d.read(count * size)
    wanted = {uuid.UUID(esp_type).bytes_le: "esp", uuid.UUID(root_type).bytes_le: "root"}
    found = set()
    for i in range(count):
        e = table[i * size:(i + 1) * size]
        name = wanted.get(e[:16])
        if not name:
            continue
        first, last = struct.unpack("<QQ", e[32:48])
        d.seek(first * 512)
        remaining = (last - first + 1) * 512
        with open(f"{work}/{name}.img", "wb") as out:
            while remaining:
                chunk = d.read(min(remaining, 1 << 22))
                out.write(chunk)
                remaining -= len(chunk)
        found.add(name)
    if found != {"esp", "root"}:
        sys.exit(f"{disk}: EFI ya da kök bölümü yok")
EOF

# BIOS yolu (Limine): çekirdek ISO9660 içinde, Live kipinde açılır.
cp "$KERNEL" "$WORK/tree/boot/banna_os"
cp "$LIMINE_DIR/limine-bios.sys" "$LIMINE_DIR/limine-bios-cd.bin" "$WORK/tree/boot/limine/"
cat > "$WORK/tree/boot/limine/limine.conf" <<'EOF'
timeout: 3

/Banna OS (Live)
    protocol: limine
    kernel_path: boot():/boot/banna_os
    cmdline: live

/Banna OS - Kurtarma kipi
    protocol: limine
    kernel_path: boot():/boot/banna_os
    cmdline: live recovery
EOF

# UEFI: El Torito EFI imajı da eklenen EFI bölümünü gösterir (aynı yükleyici).
xorriso -as mkisofs -V BANNAOS -R -J \
    -b boot/limine/limine-bios-cd.bin -no-emul-boot -boot-load-size 4 -boot-info-table \
    -append_partition 2 "$ESP_TYPE" "$WORK/esp.img" \
    -append_partition 3 "$ROOT_TYPE" "$WORK/root.img" \
    -appended_part_as_gpt \
    -eltorito-alt-boot -e --interval:appended_partition_2:all:: -no-emul-boot \
    -o "$OUT" "$WORK/tree" 2>&1 | grep -vE "^xorriso : (UPDATE|NOTE)|^Drive current|^Media current|^Media status|^Media summary|^Added to ISO|^Written to medium|^ISO image produced|^Writing to" || true
"$LIMINE_DIR/limine" bios-install "$OUT" >/dev/null 2>&1
rm -rf "$WORK"
echo "iso: $OUT ($(( $(stat -f %z "$OUT") / 1048576 )) MiB)"
