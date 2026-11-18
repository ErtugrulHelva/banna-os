#!/usr/bin/env bash
# ISO'yu QEMU'da penceresiz ve farklı donanım bileşimleriyle başlatır, seri
# logda beklenen işaretleri arar. Etkileşimli açılışlarda QEMU monitöründen
# gerçek PS/2 tuş vuruşları gönderip kabuğu, diskten ELF programlarını, ağı,
# masaüstünü (FPS ölçümüyle) ve ACPI kapatmayı da sınar.
# Çıkış 0 = temiz, 1 = regresyon. macOS bash 3.2 uyumludur.
set -u

cd "$(dirname "$0")/.."

QEMU="${QEMU:-qemu-system-x86_64}"
BOOT_SECONDS="${BOOT_SECONDS:-40}"
ISO="build/banna_os.iso"
DISK="build/regression_disk.img"
cp build/disk.img "$DISK"
# Geliştirme sırasında açılan hesaplar testin ilk kurulum akışını bozmasın.
mdel -i "$DISK" ::/etc/users >/dev/null 2>&1
QEMU_SHARE="$(brew --prefix qemu 2>/dev/null)/share/qemu"
OVMF_CODE="${OVMF_CODE:-$QEMU_SHARE/edk2-x86_64-code.fd}"
OVMF_VARS="${OVMF_VARS:-$QEMU_SHARE/edk2-i386-vars.fd}"
MIN_FPS="${MIN_FPS:-55}"

MARKERS="pmm: self-test PASS
gfx: self-test PASS
paging: self-test PASS
heap: self-test PASS
pit: self-test PASS
apic: self-test PASS
smp: self-test PASS
sched: self-test PASS
aml: self-test PASS
rtc: self-test PASS
keyboard: self-test PASS
mouse: self-test PASS
storage: self-test PASS
fs: self-test PASS
battery: self-test PASS
net: self-test PASS
usermode: self-test PASS
crypto: self-test PASS
users: self-test PASS
methods, 0 failed
idt: breakpoint at
Hello from ring 3 via syscall!
[ OK ] FPU / SSE
[ OK ] APIC (LAPIC + I/O APIC)
[ OK ] Scheduler
[ OK ] ACPI AML interpreter
[ OK ] Storage (ATA/AHCI/NVMe)
[ OK ] File system (VFS: ext2 + FAT32)
[ OK ] Display (double-buffered)
[ OK ] GPU
[ OK ] Network (Ethernet + IPv4)
net: link up
net: configured 10.0.2.15/24 gateway 10.0.2.2
[ OK ] All self-tests passed
boot: complete"

INTERACTIVE_MARKERS="login: test signed in
gui: launcher opened
gui: launched Ayarlar
settings: ready
sudo: test elevated
Paket listesi güncellendi:
Kurulacak: calc
1 paket kuruldu.
exec: /bin/calc as pid
exec: /apps/terminal/terminal as pid
exec: /bin/sh as pid
sh: ready
shell: echo şı.ŞI
layout: us
shell: echo banna-kbd-ok
banna-kbd-ok
exec: /bin/hello as pid
Hello from an ELF program!
file write/read/remove: ok
(stable)
reply from 10.0.2.2: seq=1
1 sent, 1 received
banna-tcp-ok
gui: desktop started
plasma:
gui: desktop stopped
shell: shutdown
power: shutting down"

EHCI_MARKERS="ehci: 00:
-> mass storage
fs: mounted usb0 (fat32 'EHCIUSB') at /media/usb0
[ OK ] USB (xHCI/EHCI) (EHCI, 1 device(s), mass storage)"

UHCI_MARKERS="uhci: 00:
-> keyboard
-> mouse
-> hub
-> mass storage
fs: mounted usb0 (fat32 'UHCIUSB') at /media/usb0
(UHCI, 4 device(s)"

OHCI_MARKERS="ohci: 00:
-> keyboard
-> mouse
-> hub
-> mass storage
fs: mounted usb0 (fat32 'OHCIUSB') at /media/usb0
(OHCI, 4 device(s)"

RECOVERY_MARKERS="recovery: entering recovery mode (B key)
hda: 00:
[ OK ] Audio (HD Audio) (HD Audio, line out)
BANNA OS KURTARMA
root yetkisiyle
shell: whoami
shell: shutdown
power: shutting down"

BATTERY_MARKERS="power: power button pressed
battery: \\_SB_.BAT0 present=true 75% discharging remaining=Some(3600)
battery: AC adapter Some(false)"
SMP_MARKERS="smp: 3 of 3 secondary CPU(s) online, IPI ok
paging: NX on, SMEP on, SMAP on"
Q35_MARKERS="storage: sata0
fs: mounted sata0
net: Intel 82574L"
VIRTIO_MARKERS="gpu: virtio-gpu"
NVME_MARKERS="nvme: 0 'QEMU NVMe Ctrl'
storage: nvme0n1
fs: mounted nvme0n1p2 (ext2 'BANNA') at /media/nvme0n1p2"
USB_MARKERS="usb: self-test PASS
usb: hub 'QEMU USB Hub' with 8 port(s)
-> mouse
-> keyboard
-> tablet
-> mass storage
fs: mounted usb0 (fat32 'BANNAUSB') at /media/usb0
[ OK ] USB (xHCI/EHCI)"

wait_for() {
    local log="$1" pattern="$2" pid="$3" waited=0
    while [ "$waited" -lt "$BOOT_SECONDS" ]; do
        grep -aqF -e "$pattern" "$log" 2>/dev/null && return 0
        kill -0 "$pid" 2>/dev/null || return 1
        sleep 1
        waited=$((waited + 1))
    done
    return 1
}

type_line() {
    local sock="$1" text="$2" commands="" i ch key
    i=0
    while [ "$i" -lt "${#text}" ]; do
        ch="${text:$i:1}"
        case "$ch" in
            " ") key="spc" ;;
            "-") key="minus" ;;
            ".") key="dot" ;;
            "/") key="slash" ;;
            "_") key="shift-minus" ;;
            ":") key="shift-semicolon" ;;
            *) key="$ch" ;;
        esac
        commands="${commands}sendkey ${key}
"
        i=$((i + 1))
    done
    commands="${commands}sendkey ret
"
    printf "%s" "$commands" | nc -U -w 1 "$sock" >/dev/null 2>&1
}

send_keys() {
    local sock="$1" commands="" key
    shift
    for key in "$@"; do
        commands="${commands}sendkey ${key}
"
    done
    printf "%s" "$commands" | nc -U -w 1 "$sock" >/dev/null 2>&1
}

check_markers() {
    local name="$1" log="$2" markers="$3" failed=0
    while IFS= read -r marker; do
        [ -z "$marker" ] && continue
        if ! grep -aqF -e "$marker" "$log" 2>/dev/null; then
            echo "  [$name] eksik: $marker"
            failed=1
        fi
    done <<EOF
$markers
EOF
    return "$failed"
}

# Giriş ekranı: ilk açılışta yönetici hesabı kurulur, sonrakilerde aynı hesapla girilir.
login() {
    local log="$1" sock="$2" pid="$3"
    wait_for "$log" "login: " "$pid" || return 1
    sleep 1
    if grep -aqF "login: setup required" "$log"; then
        send_keys "$sock" t e s t tab t e s t 1 2 3 4 tab t e s t 1 2 3 4 ret
    else
        send_keys "$sock" t e s t 1 2 3 4 ret
    fi
    wait_for "$log" "login: test signed in" "$pid" || return 1
    # Masaüstü terminali (ring 3 uygulaması) kabuğu başlatana kadar bekle.
    wait_for "$log" "sh: ready" "$pid" || return 1
    # İlk oturumda kurulum sihirbazı terminalin üstünde açılır: Esc ile atla
    # (aynı diskle sonraki açılışlarda sihirbaz bir daha gelmez).
    if BOOT_SECONDS=6 wait_for "$log" "setup: ready" "$pid"; then
        sleep 1
        send_keys "$sock" esc
        wait_for "$log" "setup: finished" "$pid"
        sleep 1
    fi
}

interactive() {
    local name="$1" log="$2" sock="$3" pid="$4" failed=0
    login "$log" "$sock" "$pid" || echo "  [$name] giriş başarısız"
    send_keys "$sock" e c h o spc semicolon i slash shift-semicolon shift-i ret
    wait_for "$log" "shell: echo" "$pid"
    type_line "$sock" "keymap us"
    wait_for "$log" "layout: us" "$pid"
    type_line "$sock" "echo banna-kbd-ok"
    wait_for "$log" "shell: echo banna-kbd-ok" "$pid"
    type_line "$sock" "hello"
    wait_for "$log" "file write/read/remove" "$pid"
    type_line "$sock" "fputest 2"
    wait_for "$log" "fputest seed 2" "$pid"
    type_line "$sock" "ping 10.0.2.2 1"
    wait_for "$log" "1 sent" "$pid"
    type_line "$sock" "http 10.0.2.2:$HTTP_PORT/tcp.txt"
    wait_for "$log" "http: " "$pid"
    # sudo + hatch: root kabuğunda depo adresi, liste güncelleme ve bağımlılıklı kurulum.
    type_line "$sock" "sudo sh"
    wait_for "$log" "parolas" "$pid"
    type_line "$sock" "test1234"
    wait_for "$log" "sudo: test elevated" "$pid"
    sleep 1
    type_line "$sock" "hatch source http://10.0.2.2:$HTTP_PORT/repo/"
    wait_for "$log" "shell: hatch source" "$pid"
    type_line "$sock" "hatch update"
    wait_for "$log" "Paket listesi g" "$pid"
    # Aynı test diski önceki açılışlardan paket taşıyabilir.
    type_line "$sock" "hatch remove calc"
    wait_for "$log" "shell: hatch remove" "$pid"
    sleep 1
    type_line "$sock" "hatch install calc"
    wait_for "$log" "paket kuruldu" "$pid"
    type_line "$sock" "exit"
    sleep 1
    type_line "$sock" "calc 6*7"
    wait_for "$log" "shell: calc" "$pid"
    type_line "$sock" "desktop"
    wait_for "$log" "gui: desktop started" "$pid"
    type_line "$sock" "plasma 180"
    wait_for "$log" "plasma: " "$pid"
    local fps
    fps=$(grep -a "plasma: " "$log" | tail -1 | sed -E 's/.*\(([0-9]+) fps\).*/\1/')
    if [ -z "$fps" ] || [ "$fps" -lt "$MIN_FPS" ]; then
        echo "  [$name] plasma FPS '$fps' < $MIN_FPS"
        failed=1
    else
        echo "  [$name] plasma (ring 3 window): $fps fps"
    fi
    # Başlatıcı (Super tuşu) ve aramayla Ayarlar uygulaması.
    send_keys "$sock" meta_l
    wait_for "$log" "gui: launcher opened" "$pid"
    sleep 0.5
    send_keys "$sock" a y a r ret
    wait_for "$log" "settings: ready" "$pid"
    sleep 0.5
    send_keys "$sock" esc
    sleep 0.5
    type_line "$sock" "console"
    wait_for "$log" "gui: desktop stopped" "$pid"
    type_line "$sock" "shutdown"
    local waited=0
    while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 10 ]; do
        sleep 1
        waited=$((waited + 1))
    done
    if kill -0 "$pid" 2>/dev/null; then
        echo "  [$name] 'shutdown' QEMU'yu kapatmadı"
        failed=1
    fi
    check_markers "$name" "$log" "$INTERACTIVE_MARKERS" || failed=1
    return "$failed"
}

# Kurtarma kipi: açılış boyunca B'ye basılır; masaüstü açılmamalı, kabuk metin konsolundadır.
recovery() {
    local name="$1" log="$2" sock="$3" pid="$4" failed=0
    wait_for "$log" "login: " "$pid" || return 1
    sleep 1
    # Açılışta basılan fazladan B'ler giriş satırına düşmüş olabilir.
    send_keys "$sock" backspace backspace backspace backspace backspace backspace
    type_line "$sock" "test"
    sleep 1
    type_line "$sock" "test1234"
    if grep -aqF "login: setup required" "$log"; then
        sleep 1
        type_line "$sock" "test1234"
    fi
    wait_for "$log" "root yetkisiyle" "$pid" || failed=1
    type_line "$sock" "keymap us"
    wait_for "$log" "layout: us" "$pid"
    type_line "$sock" "whoami"
    wait_for "$log" "shell: whoami" "$pid"
    type_line "$sock" "shutdown"
    local waited=0
    while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 10 ]; do
        sleep 1
        waited=$((waited + 1))
    done
    if grep -aqF "gui: desktop started" "$log"; then
        echo "  [$name] kurtarma kipinde masaüstü açıldı"
        failed=1
    fi
    return "$failed"
}

LIVE_MARKERS="loader: Banna OS UEFI loader
smp: 3 of 3 secondary CPU(s) online, IPI ok
loader: cmdline: live root=PARTUUID=
fs: mounted usb0p2 (ext2 'BANNA') at /
fs: mounted usb0p1 (fat32 'BANNAEFI') at /boot
login: live signed in
sh: ready
sudo: live elevated
banna-install: step 5
banna-install: complete
boot: complete"

INSTALLED_MARKERS="loader: Banna OS UEFI loader
fs: mounted usb0p2 (ext2 'BANNA') at /
fs: mounted usb0p1 (fat32 'BANNAEFI') at /boot
users: 0 account(s)
login: setup required
boot: complete"

# Kendi UEFI önyükleyicimizle USB bellekten açılış (CD ve sabit disk yok).
boot_usb() {
    local name="$1" log="$2" sock="$3" first="$4" second="$5"
    cp "$OVMF_CODE" "build/${name}_code.fd"
    cp "$OVMF_VARS" "build/${name}_vars.fd"
    local extra=""
    if [ -n "$second" ]; then
        extra="-drive if=none,id=stick2,file=$second,format=raw -device usb-storage,bus=xhci.0,drive=stick2"
    fi
    "$QEMU" -m 512M -smp 4 -cpu max -display none -no-reboot -serial "file:$log" \
        -monitor "unix:$sock,server,nowait" \
        -drive if=pflash,format=raw,readonly=on,file="build/${name}_code.fd" \
        -drive if=pflash,format=raw,file="build/${name}_vars.fd" \
        -device qemu-xhci,id=xhci \
        -drive if=none,id=stick1,file="$first",format=raw \
        -device usb-storage,bus=xhci.0,drive=stick1,bootindex=1 \
        $extra $NET >/dev/null 2>&1 &
    echo $!
}

finish_boot() {
    local name="$1" log="$2" sock="$3" pid="$4" markers="$5" failed="$6"
    kill "$pid" 2>/dev/null
    wait "$pid" 2>/dev/null
    rm -f "$sock"
    check_markers "$name" "$log" "$markers" || failed=1
    if grep -aqE "FAIL|PANIC|EXCEPTION" "$log" 2>/dev/null; then
        echo "  [$name] hata satırları:"
        grep -aE "FAIL|PANIC|EXCEPTION" "$log" | sed 's/^/    /'
        failed=1
    fi
    if [ "$failed" -eq 0 ]; then
        echo "PASS  $name ($log)"
    else
        echo "FAIL  $name ($log)"
    fi
    return "$failed"
}

# Live USB → banna-install ile ikinci USB'ye kurulum → kurulan USB'den açılış.
run_live() {
    local live="build/regression_live.img" target="build/regression_target.img"
    local log="build/regression_live.log" sock="build/monitor_live.sock" failed=0 pid
    cp "$DISK" "$live"
    rm -f "$target" "$log" "$sock"
    dd if=/dev/zero of="$target" bs=1048576 count=0 seek=1024 2>/dev/null
    pid=$(boot_usb live "$log" "$sock" "$live" "$target")
    wait_for "$log" "sh: ready" "$pid" || failed=1
    sleep 1
    type_line "$sock" "keymap us"
    wait_for "$log" "layout: us" "$pid"
    type_line "$sock" "sudo banna-install"
    wait_for "$log" "parolas" "$pid"
    type_line "$sock" "live"
    wait_for "$log" "disk numaras" "$pid"
    type_line "$sock" "1"
    wait_for "$log" "disk ad" "$pid"
    type_line "$sock" "usb1"
    wait_for "$log" "banna-install: complete" "$pid" || failed=1
    finish_boot live "$log" "$sock" "$pid" "$LIVE_MARKERS" "$failed" || return 1

    log="build/regression_installed.log"
    sock="build/monitor_installed.sock"
    rm -f "$log" "$sock"
    failed=0
    pid=$(boot_usb installed "$log" "$sock" "$target" "")
    wait_for "$log" "login: setup required" "$pid" || failed=1
    # hatch update/upgrade: imzalı sistem güncellemesi (yeni çekirdek dahil) hazırlanır,
    # sonraki açılışta kurulur.
    login "$log" "$sock" "$pid" || failed=1
    type_line "$sock" "keymap us"
    wait_for "$log" "layout: us" "$pid"
    type_line "$sock" "sudo sh"
    wait_for "$log" "parolas" "$pid"
    type_line "$sock" "test1234"
    wait_for "$log" "sudo: test elevated" "$pid"
    sleep 1
    type_line "$sock" "hatch source http://10.0.2.2:$HTTP_PORT/repo/"
    wait_for "$log" "shell: hatch source" "$pid"
    type_line "$sock" "hatch update"
    wait_for "$log" "Sistem güncellemesi: " "$pid" || failed=1
    type_line "$sock" "hatch upgrade"
    wait_for "$log" "Yeniden başlatınca kurulacak" "$pid" || failed=1
    finish_boot installed "$log" "$sock" "$pid" "$INSTALLED_MARKERS" "$failed" || return 1

    # Bekleyen güncelleme açılışta kurulur; çekirdek değiştiği için sistem yeniden başlar
    # (-no-reboot ile QEMU kapanır).
    log="build/regression_applying.log"
    sock="build/monitor_applying.sock"
    rm -f "$log" "$sock"
    failed=0
    pid=$(boot_usb applying "$log" "$sock" "$target" "")
    wait_for "$log" "update: installed, restarting" "$pid" || failed=1
    finish_boot applying "$log" "$sock" "$pid" "banna-update: applied regtest" "$failed" || return 1

    log="build/regression_updated.log"
    sock="build/monitor_updated.sock"
    rm -f "$log" "$sock"
    failed=0
    pid=$(boot_usb updated "$log" "$sock" "$target" "")
    wait_for "$log" "login: " "$pid" || failed=1
    finish_boot updated "$log" "$sock" "$pid" "loader: kernel: \\boot\\banna_os ($UPDATE_KERNEL_SIZE bytes)
fs: mounted usb0p1 (fat32 'BANNAEFI') at /boot" "$failed"
}

run_boot() {
    local name="$1" mode="$2" extra="$3"
    shift 3
    local log="build/regression_${name}.log"
    local sock="build/monitor_${name}.sock"
    rm -f "$log" "$sock"

    "$QEMU" -m 256M -display none -no-reboot -serial "file:$log" \
        -monitor "unix:$sock,server,nowait" -boot d -cdrom "$ISO" \
        -drive "file=$DISK,format=raw,if=ide,index=0" "$@" >/dev/null 2>&1 &
    local pid=$!
    local failed=0

    if [ "$mode" = "recovery" ]; then
        local waited=0
        while ! grep -aqF "boot: complete" "$log" 2>/dev/null && [ "$waited" -lt 200 ]; do
            sleep 0.2
            send_keys "$sock" b
            waited=$((waited + 1))
        done
    fi
    wait_for "$log" "boot: complete" "$pid" || failed=1
    wait_for "$log" "net: configured" "$pid"
    if [ "$failed" -eq 0 ] && [ "$mode" = "interactive" ]; then
        interactive "$name" "$log" "$sock" "$pid" || failed=1
    fi
    if [ "$failed" -eq 0 ] && [ "$mode" = "recovery" ]; then
        recovery "$name" "$log" "$sock" "$pid" || failed=1
    fi
    if [ "$failed" -eq 0 ] && [ "$mode" = "powerbutton" ]; then
        # ACPI güç düğmesi: sistem kendini düzgünce kapatmalı.
        sleep 2
        printf "system_powerdown\n" | nc -U -w 1 "$sock" >/dev/null 2>&1
        local waited=0
        while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 15 ]; do
            sleep 1
            waited=$((waited + 1))
        done
        if kill -0 "$pid" 2>/dev/null; then
            echo "  [$name] güç düğmesi sistemi kapatmadı"
            failed=1
        fi
    fi

    kill "$pid" 2>/dev/null
    wait "$pid" 2>/dev/null
    rm -f "$sock"

    check_markers "$name" "$log" "$MARKERS" || failed=1
    if [ -n "$extra" ]; then
        check_markers "$name" "$log" "$extra" || failed=1
    fi
    if grep -aqE "FAIL|PANIC|EXCEPTION" "$log" 2>/dev/null; then
        echo "  [$name] hata satırları:"
        grep -aE "FAIL|PANIC|EXCEPTION" "$log" | sed 's/^/    /'
        failed=1
    fi

    if [ "$failed" -eq 0 ]; then
        echo "PASS  $name ($log)"
    else
        echo "FAIL  $name ($log)"
    fi
    return "$failed"
}

if [ ! -f "$ISO" ] || [ ! -f "$DISK" ]; then
    echo "ISO ya da disk bulunamadı: önce 'make image disk' çalıştır." >&2
    exit 1
fi

python3 scripts/gen_test_battery_ssdt.py build/test-battery.aml || exit 1
NET="-netdev user,id=net0 -device e1000,netdev=net0"

# TCP testi için ana makinede küçük bir HTTP sunucusu (misafirden 10.0.2.2).
HTTP_PORT="${HTTP_PORT:-18765}"
HTTP_ROOT="build/www"
mkdir -p "$HTTP_ROOT"
echo "banna-tcp-ok" > "$HTTP_ROOT/tcp.txt"
rm -rf "$HTTP_ROOT/repo"
cp -R build/repo "$HTTP_ROOT/repo"
# Sistem güncellemesi testi: çekirdeğin sonuna dolgu eklenir (ELF yine geçerli, boyutu farklı).
cp build/banna_os.stripped build/banna_os.regtest
dd if=/dev/zero bs=4096 count=1 >> build/banna_os.regtest 2>/dev/null
UPDATE_KERNEL_SIZE=$(wc -c < build/banna_os.regtest | tr -d ' ')
python3 scripts/build_system.py "$HTTP_ROOT/repo/system" --version regtest \
    --file /boot/boot/banna_os=build/banna_os.regtest >/dev/null || exit 1
python3 -m http.server "$HTTP_PORT" --bind 127.0.0.1 --directory "$HTTP_ROOT" >/dev/null 2>&1 &
HTTP_PID=$!
trap 'kill $HTTP_PID 2>/dev/null' EXIT

status=0
run_boot bios interactive "" $NET || status=1
run_boot smp4-smep boot "$SMP_MARKERS" -smp 4 -cpu max $NET || status=1
run_boot q35 boot "$Q35_MARKERS" -machine q35 -netdev user,id=net0 -device e1000e,netdev=net0 || status=1
run_boot virtio-gpu boot "$VIRTIO_MARKERS" -vga virtio $NET || status=1
NVME_IMG="build/nvme_regression.img"
cp "$DISK" "$NVME_IMG"
run_boot nvme boot "$NVME_MARKERS" -machine q35 -drive "file=$NVME_IMG,format=raw,if=none,id=nv" \
    -device nvme,serial=banna01,drive=nv $NET || status=1
USB_IMG="build/usb_regression.img"
rm -f "$USB_IMG"
dd if=/dev/zero of="$USB_IMG" bs=1048576 count=40 2>/dev/null
mformat -i "$USB_IMG" -F -v BANNAUSB :: >/dev/null 2>&1
run_boot usb boot "$USB_MARKERS" -device qemu-xhci,id=xhci -device usb-kbd,bus=xhci.0 \
    -device usb-tablet,bus=xhci.0 -drive if=none,id=stick,file=$USB_IMG,format=raw \
    -device usb-storage,bus=xhci.0,drive=stick -device usb-hub,bus=xhci.0,port=4 \
    -device usb-mouse,bus=xhci.0,port=4.2 $NET || status=1
EHCI_IMG="build/ehci_regression.img"
rm -f "$EHCI_IMG"
dd if=/dev/zero of="$EHCI_IMG" bs=1048576 count=40 2>/dev/null
mformat -i "$EHCI_IMG" -F -v EHCIUSB :: >/dev/null 2>&1
run_boot ehci boot "$EHCI_MARKERS" -device usb-ehci,id=ehci -drive if=none,id=es,file=$EHCI_IMG,format=raw \
    -device usb-storage,bus=ehci.0,drive=es $NET || status=1
# USB 1.1 denetleyicileri: klavye + doğrudan bağlı fare/bellek + hub arkasında cihaz.
for hc in uhci ohci; do
    HC_IMG="build/${hc}_regression.img"
    rm -f "$HC_IMG"
    dd if=/dev/zero of="$HC_IMG" bs=1048576 count=40 2>/dev/null
    label=$(echo "$hc" | tr '[:lower:]' '[:upper:]')
    mformat -i "$HC_IMG" -F -v "${label}USB" :: >/dev/null 2>&1
    if [ "$hc" = uhci ]; then hcdev=piix3-usb-uhci; markers="$UHCI_MARKERS"; else hcdev=pci-ohci; markers="$OHCI_MARKERS"; fi
    run_boot "$hc" boot "$markers" -device $hcdev,id=hc -device usb-kbd,bus=hc.0 -device usb-hub,bus=hc.0 \
        -device usb-mouse,bus=hc.0,port=2.1 -drive if=none,id=hs,file=$HC_IMG,format=raw \
        -device usb-storage,bus=hc.0,port=2.2,drive=hs $NET || status=1
done
RECOVERY_WAV="build/regression_recovery.wav"
rm -f "$RECOVERY_WAV"
run_boot recovery recovery "$RECOVERY_MARKERS" -audiodev wav,id=snd0,path=$RECOVERY_WAV \
    -device intel-hda -device hda-output,audiodev=snd0 $NET || status=1
# Kurtarma kipine girişte 880 + 660 Hz bip çalınır (ortalama ~700-800 Hz).
if ! python3 scripts/check_audio.py "$RECOVERY_WAV" 600 900; then
    echo "FAIL  recovery-beep ($RECOVERY_WAV)"
    status=1
else
    echo "PASS  recovery-beep"
fi
run_boot battery powerbutton "$BATTERY_MARKERS" -acpitable file=build/test-battery.aml $NET || status=1

if [ -f "$OVMF_CODE" ]; then
    mkdir -p build
    cp "$OVMF_CODE" build/OVMF_CODE.fd
    cp "$OVMF_VARS" build/OVMF_VARS.fd
    run_boot uefi interactive "" \
        -drive if=pflash,format=raw,readonly=on,file=build/OVMF_CODE.fd \
        -drive if=pflash,format=raw,file=build/OVMF_VARS.fd $NET || status=1
    run_live || status=1
else
    echo "SKIP  uefi, live (OVMF bulunamadı: $OVMF_CODE)"
fi

exit "$status"
