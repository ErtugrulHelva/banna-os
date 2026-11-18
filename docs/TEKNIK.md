# Banna OS — teknik ayrıntılar

Rust ile yazılan x86_64 işletim sistemi. Önyükleyici Limine (base revision 3);
ISO hem BIOS hem UEFI ile açılır. Çekirdekte ve kullanıcı kütüphanesinde dış crate yoktur.

## Dizin düzeni

- `src/boot/`: Limine istekleri (HHDM, framebuffer, bellek haritası, RSDP, MP, yığın boyutu)
- `src/arch/x86_64/`: GDT/TSS (CPU başına), IDT, PIC, PIT, TSC, LAPIC + I/O APIC, SMP, FPU/SSE, CPUID, `syscall`
- `src/memory/`: fiziksel çerçeve ayırıcı, sayfalama (NX/SMEP/SMAP, PAT/WC), adres alanları, çekirdek yığınları, heap
- `src/sched/`: preemptif zamanlayıcı, işlemler (dosya tanıtıcıları, mmap), bekleme kuyrukları, mutex
- `src/aml/`: ACPI AML yorumlayıcısı
- `src/storage/`: blok katmanı, MBR/GPT, ATA PIO, AHCI, NVMe
- `src/fs/`: VFS ve FAT32 (okuma/yazma, uzun adlar)
- `src/drivers/`: seri, konsol, PS/2 klavye/fare, RTC, ACPI, EC, PCI, güç, batarya, virtio, GPU (BGA, virtio-gpu), e1000
- `src/net/`: Ethernet, ARP, IPv4, ICMP, UDP, TCP, DHCP istemcisi, DNS çözücü
- `src/gfx/`, `src/gui/`: 2B çizim, çift tamponlu ekran, olay güdümlü compositor, panel, dock ve başlatıcı
  (`canvas.rs`, `text.rs`, `fast.rs`, `icons.rs`, `theme.rs`, `wallpaper_art.rs` kullanıcı kütüphanesiyle ortaktır)
- `loader/`: UEFI önyükleyici (BOOTX64.EFI)
- `tools/mkimage.rs`: GPT + ext2 disk imajı aracı (ana makine)
- `tools/brct.rs`: brct — Banna OS için Rust uygulaması derleyicisi (ana makine); `examples/`: örnek uygulama
- `src/kernel/`: initcall, syscall tablosu, ELF yükleyici, pipe ve pty, gömülü test programları, çekirdek kabuğu, kurtarma kipi
- `apps/`: masaüstü uygulamaları, her biri kendi klasöründe (`app.conf` + `main.rs`); kurulum `/apps/<kimlik>/`.
  Ayrıntılar: `apps/README.md`
- `user/`: `libbanna` çalışma zamanı (`gui`, `gfx`, `ui`, `desktop`...) ve komut satırı programları (`sh`, `edit`, `sudo`, `hatch`, `lsblk`, `mount`, `gpt`, `mkfs.ext2`, `mkfs.fat`, `banna-install`, `chmod`, `chown`, `mv`, `cp`, `ls`, `cat`, `grep`, `wc`, `mkdir`, `rm`, `touch`, `hello`, `sysinfo`, `fputest`, `http`, `tcpecho`)
- `packages/`: hatch deposuna giren paket tanımları (`package.ini` + `files/`)
- `scripts/`: regresyon testi, sahte batarya SSDT üreticisi, depo derleyici (`build_repo.py`)

## Açılış sırası (initcall)

| Seviye | Alt sistem |
| --- | --- |
| arch | GDT/TSS, IDT, FPU/SSE, syscall MSR'leri |
| core | PMM, sayfalama, heap, ACPI tabloları, PIT + TSC, APIC, SMP, zamanlayıcı |
| driver | PCI, AML + EC, RTC, klavye, fare, depolama, dosya sistemi, güç, batarya, ekran, GPU, ağ |
| late | ring 3 işlem testi |

`early`/`arch`/`core` hatası açılışı durdurur; `driver`/`late` hatası `[FAIL]` olarak raporlanır.

## Kullanıcı programları

`user/` ayrı bir Cargo projesidir; programlar statik ELF olarak `0x400000`'a bağlanır ve
SSE kullanabilir (çekirdek FPU durumunu thread başına FXSAVE ile saklar). `make disk`
programları FAT32 imajındaki `/bin` altına kopyalar; kabukta adlarıyla çalışırlar
(`hello a b`, `plasma &`, `ls /bin | grep sh`). Yeni program için `user/src/bin/<ad>.rs` ekleyip
`Makefile`'daki `USER_BINS` listesine yazmak yeterlidir.

```rust
#![no_std]
#![no_main]
banna::entry!(main);
fn main(args: &[&str]) -> i32 { banna::println!("{:?}", args); 0 }
```

## Syscall ABI

`syscall`; `rax` = numara, argümanlar `rdi, rsi, rdx, r10, r8, r9`, dönüş `rax`
(negatif = POSIX errno). Kullanıcı işaretçileri sayfa tablosundan doğrulanır, SMAP varsa
`stac`/`clac` ile kopyalanır.

| No | Ad | No | Ad |
| --- | --- | --- | --- |
| 0 | exit(code) | 13 | mkdir(path) |
| 1 | write(fd, buf, len) | 14 | unlink(path) |
| 2 | read(fd, buf, len) | 15 | spawn(path, argv, argc) → pid |
| 3 | uptime_ms | 16 | wait(pid) → kod |
| 4 | sleep_ms(ms) | 17 | mmap(len) → adres |
| 5 | time → Unix | 18 | munmap(addr, len) |
| 6 | getpid | 19 | sysinfo(&out) |
| 7 | yield | 20 | win_create(w, h, title) → id |
| 8 | open(path, flags) → fd | 21 | win_present(id, pixels, w, h) |
| 9 | close(fd) | 22 | win_event(id, &event, timeout_ms) |
| 10 | seek(fd, pos) | 23 | win_close(id) |
| 11 | stat(path, &out) | 24 | kill(pid) |
| 12 | readdir(fd, i, &out) | 25 | time_ns (TSC) |

| No | Ad | No | Ad |
| --- | --- | --- | --- |
| 16 | wait(pid, flags) → kod (flags 1 = beklemeden, EAGAIN) | 38 | getcwd(buf, len) → uzunluk |
| 24 | kill(pid, sinyal) (pid < 0 = süreç grubu, sinyal 0 = SIGKILL) | 39 | setpgid(pid, pgid) |
| 26-30 | connect, listen, accept, resolve, recv | 40 | getpgid(pid) |
| 31 | getuid | 41 | sigignore(maske) → eski maske |
| 32 | username(buf, len) | 42 | openpty(&[ana, yardımcı]) |
| 33 | fork → çocukta 0, ebeveynde pid | 43 | tcsetpgrp(fd, pgid) |
| 34 | exec(path, argv, argc) (yalnızca hatada döner) | 44 | ttymode(fd, kip) → eski kip |
| 35 | pipe(&[okuma, yazma]) | 45 | poll(&girdiler, n, timeout_ms) → hazır sayısı |
| 36 | dup2(eski, yeni) | 46 | log(buf, len) (seri günlük) |
| 37 | chdir(path) | 47 | kcmd(satır, len) (çekirdek tanı komutu, çıktı stdout'a) |
| 48 | getppid | 49 | tcgetpgrp(fd) |
| 50 | elevate(parola, len) (yönetici → root; EPERM/EACCES) | 51 | netinfo(&out) (adres, maske, ağ geçidi, DNS) |
| 52 | ttysize(fd, sütun \| satır << 16) (u32::MAX = oku) | 53 | rename(eski, yeni) |
| 54 | chmod(path, kip) (sahip ya da root) | 55 | chown(path, uid, gid) (root) |
| 56 | mount(aygıt, dizin) (root) | 57 | umount(dizin) (root) |
| 58 | rescan(disk) → birim sayısı (root) | 59 | statfs(path, &[toplam, boş]) |
| 60 | info(konu, buf, len): 0 komut satırı, 1 disk/bölüm/bağlama raporu | | |

## İşlemler

- `fork` adres alanını copy-on-write paylaşır: yazılabilir sayfalar iki tarafta salt okunur +
  CoW bitiyle işaretlenir, çerçeve sahip sayısı PMM'de tutulur, ilk yazmada sayfa hataları
  kopyalanır. Çocuk ebeveynin syscall yazmaçları ve FPU/SSE durumuyla `rax = 0` döner.
- `exec` yeni görüntüyü ayrı bir adres alanına yükler; başarısız olursa eski görüntü bozulmaz.
  Tanıtıcılar, dizin ve süreç grubu korunur.
- Tanıtıcılar: konsol, dosya, TCP soketi, pipe uçları, pty uçları. `spawn` ile başlatılan program
  0-2 tanıtıcılarını, çalışma dizinini, kimliği ve süreç grubunu çağırandan devralır.
- Sinyaller şimdilik yalnızca sonlandırır (çıkış kodu `-sinyal`); işlem bir sinyali yok sayabilir
  (SIGKILL hariç). Bloklu `read`/`write`/`poll` sinyalle `EINTR` ile döner.

## Terminal ve kabuk

Masaüstünde terminal bir ring 3 uygulamasıdır (`/bin/terminal`): oturum açılınca kendiliğinden
başlar, dock'taki "T" yenisini açar. Pencere + pty kurar, `fork` ile `/bin/sh` çalıştırır; ANSI/VT100
alt kümesini (imleç, silme, SGR renkleri) çizer, PageUp/PageDown ile 1000 satır geri kaydırır.

pty satır disiplini: kanonik kipte satır düzenleme ve Enter'da teslim, yankı, Ctrl+C / Ctrl+\
ile ön plan süreç grubuna SIGINT / SIGQUIT, boş satırda Ctrl+D = dosya sonu. Programların çıktısı
seri günlüğe de yansıtılır.

`sh`: satır düzenleme (oklar, Home/End, Ctrl+A/E/U/K/L), geçmiş, Tab tamamlama, `a | b`,
`> >> <`, `&& || ;`, `&` ile arka plan işleri (`jobs`, `kill %1`), `~` ve `$?`, `sh -c` ve betik
dosyaları. Yerleşikler: `cd pwd echo clear kill jobs history help exit`. `/bin`'de olmayan tanı
komutları (`ps`, `mem`, `net`, `ping`, `lsusb`, `df`, `console`, `reboot`...) çekirdekten `kcmd`
ile çalışır; sistemi değiştiren komutlar (hesaplar, `sudo`, `dhcp`, `aml eval`, `gpu mode`)
yalnızca kurtarma kipindedir.

## Metin editörü

`edit [dosya]` nano benzeri bir terminal editörüdür: oklar, Home/End, PageUp/PageDown ile gezinme,
otomatik girinti, UTF-8. Kısayollar: `^S` kaydet, `^Q` çık (kaydedilmemişse iki kez), `^F` bul
(tekrar `^F` sonrakini bulur), `^G` satıra git, `^K` satırı kes (art arda kesilenler birikir),
`^U` yapıştır. Terminal alternatif ekranı (`ESC[?1049h`) destekler; editörden çıkınca önceki
ekran geri gelir.

## sudo

`sudo <komut>` yönetici hesabının parolasını yankısız sorar; çekirdek (`elevate`) parolayı
doğrularsa yalnızca o işlem ve başlattıkları root olur. `sudo sh` root kabuğu açar.

## Paketler (hatch)

| Komut | Görev |
| --- | --- |
| `sudo hatch update` | Paket listesini depodan indirir |
| `sudo hatch install <paket>...` | Bağımlılıklarıyla kurar |
| `sudo hatch upgrade` | Kurulu paketleri yeni sürümlere yükseltir |
| `sudo hatch remove <paket>...` | Kaldırır (başka paketin bağımlılığıysa reddeder) |
| `hatch list`, `hatch search [metin]`, `hatch info <paket>` | Kurulu paketler, arama, ayrıntı |
| `hatch diagnose` | Donanımları, modelleri ve sürücüleri listeler (`/usr/share/hwdata`) |
| `hatch diagnose --send` | Aydınlatma metni kabul edildikten sonra donanım listesini Banna hesabı üzerinden gönderir |
| `hatch source [adres]` | Depo adresini gösterir / ayarlar |

Depo HTTP ya da HTTPS üzerinden okunur (liste imzalıdır): `<kaynak>/index` listesi ve `<kaynak>/pkgs/<ad>-<sürüm>.hpk`
paketleri. Her paket indirilince boyutu ve SHA-256 özeti listeyle karşılaştırılır; paketler
yalnızca `/bin`, `/usr` ve kendi `/etc/<paket>/` dizinine yazabilir. Varsayılan kaynak
`http://@gateway:8080/`: `@gateway` DHCP ağ geçididir, yani QEMU'da (10.0.2.2) ve UTM'de ana
makine. Durum `/etc/hatch/sources` ve `/var/hatch/` altında tutulur.

Depoyu Mac'te üretip sunmak için:

```sh
make repo         # packages/ → build/repo (index + .hpk)
make repo-serve   # build/repo'yu 0.0.0.0:8080'de sunar
```

Yeni paket: `packages/<ad>/package.ini` (`version`, `description`, `depends = a, b`,
`bin = <user programı>`) ve isteğe bağlı `packages/<ad>/files/` ağacı (ör.
`files/usr/share/...`). Sürüm artırılıp `make repo` çalıştırılınca misafirde `sudo hatch update` ve
`sudo hatch upgrade` yeni sürümü kurar. Depodaki paketler: `banna-panel`, `banna-search`,
`driver-manager` ve `calc`.

## Kurtarma kipi

Açılışta (logo ekranındayken) **B** tuşuna basılı tutulursa — ya da çekirdek komut satırında
`recovery` varsa — kurtarma kipi açılır: masaüstü başlamaz, tam ekran metin konsolunda yönetici
hesabıyla giriş istenir ve çekirdek kabuğu root yetkisiyle tüm komutlarla çalışır (`useradd`,
`userdel`, `passwd`, `dhcp`, `gpu mode`, `aml eval`, `umount`...). Normal açılışa `reboot` ile dönülür.
Kurtarma kipine girildiğinde iki tonlu bip çalınır (HD Audio; yoksa PC hoparlörü). B, logo
ekranı boyunca (geç tanınan USB klavyeler dahil) dinlenir.

Kurtarma araçları:

| Komut | İş |
|---|---|
| `dmesg [satır]` / `dmesg save [yol]` | çekirdek günlüğü; ESP'ye kaydetme |
| `hwinfo` | donanım özeti (işlemci P/E çekirdekleri, bellek, PCI, diskler) |
| `bootcfg [timeout <sn> \| cmdline <metin> \| add <bayrak> \| remove <bayrak>]` | önyükleme ayarları (`/boot/boot/banna.cfg`) |
| `rollback` | önceki çekirdeği (`banna_os.old`) varsayılan yapar |
| `backup <klasör>` / `restore <klasör>` | `/home` ve `/etc` yedeği (ör. USB belleğe) |
| `factory-reset` | hesapları, ev klasörlerini ve ayarları siler (`RESET` onayıyla) |
| `reset-desktop` | masaüstü ayarlarını varsayılana döndürür |

## Depolama ve dosya sistemi

Sürücüler: ATA PIO (IDE), AHCI (SATA), NVMe, USB yığın bellek; MBR ve GPT bölümleri (GPT tip ve
benzersiz kimlikleriyle). Dosya sistemleri:

- **ext2** (`src/fs/ext2.rs`): Linux uyumlu okuma/yazma; dolaylı bloklar, dizinler, taşıma,
  Unix izinleri (sahip/grup/diğer, üst dizinlerde arama izni, `/tmp`'de yapışkan bit). Linux'ta
  `mke2fs` ile biçimlenmiş ext2 ve temiz kapatılmış ext3 de kullanılır; ext4 (extent) reddedilir.
  Aynı dosyayı çekirdek, ana makinedeki `tools/mkimage.rs` ve kullanıcı alanındaki `mkfs.ext2`
  derler; çıktısı `e2fsck` ile denetlenmiştir.
- **FAT32**: EFI bölümü ve taşınabilir bellekler için (izin bitleri yok, yol kuralları geçerli).
- **/dev**: `null`, `zero` ve her disk/bölüm (`/dev/usb1`, `/dev/sata0p2`); bloklar yalnızca root.

Kök (`/`): komut satırındaki `root=PARTUUID=<guid>`, yoksa GPT'de "Linux root (x86-64)" tipli
bölüm, o da yoksa ilk sabit birim. USB kökü geç gelirse 10 sn beklenir. Kökle aynı diskteki EFI
bölümü `/boot`'a, diğer birimler `/media/<aygıt>` altına bağlanır (takılınca kendiliğinden).

Dizin düzeni: `/bin /boot /dev /etc /home /media /mnt /root /tmp /usr /var`.

| Program | Görev |
| --- | --- |
| `lsblk` | Diskler, bölümler, PARTUUID'ler ve bağlama noktaları |
| `mount [aygıt dizin]`, `umount dizin` | Bağlı birimler / bağlama (root) |
| `sudo gpt new usb1 [ext2\|fat] [etiket]` | Diski tek bölümlü GPT yapar, biçimler, `/media/usb1p1`'e bağlar |
| `gpt usb1` | Bölüm tablosunu gösterir |
| `sudo mkfs.ext2 /dev/usb1p1`, `sudo mkfs.fat /dev/usb1p1` | Biçimlendirir (onay ister, `-y` sormaz) |
| `ls -l`, `chmod 755 f`, `sudo chown 1000:1000 f`, `mv`, `cp -r` | Dosya işlemleri |
| `sudo banna-install` | Sistemi başka bir diske kurar (aşağıda) |

## Önyükleyici (UEFI)

`loader/` kendi UEFI önyükleyicimizdir (`EFI/BOOT/BOOTX64.EFI`, dış crate yok). EFI bölümündeki
`/boot/banna.cfg` ayarlarını (`timeout=3`, `cmdline=live`) okur, 3 sn'lik menü gösterir
(1 Banna OS, 2 Kurtarma kipi, 3 Metin konsolu; oklar/Enter, `B` = kurtarma), kendi diskindeki
Linux kök bölümünü bulup `root=PARTUUID=` ekler, çekirdeği (`/boot/banna_os`) yükler ve çekirdeğe
Limine protokolüyle aynı yanıtları verir (HHDM, bellek haritası, GOP framebuffer, RSDP, yığın,
komut satırı). Çok büyük ya da küçük ekran kiplerinde ≤ 2560x1600 olan en iyi kipe geçer.
Seri porta `loader: ...` satırları yazar. BIOS'lu eski PC'ler için aynı imajda Limine de kuruludur.

## Live USB ve kurulum

**En kolayı:** `make iso` → `build/BannaOS.iso` (hibrit imaj, ~514 MiB). USB'ye *ham* yazılır:

- **balenaEtcher** (Mac/Windows/Linux): imajı ve USB'yi seç, "Flash".
- **Rufus** (Windows): imajı seç, "Başlat"a basınca çıkan *ISOHybrid* sorusunda
  **"DD Görüntüsü kipinde yaz" (DD Image mode)** seçilmeli. "ISO kipi" seçilirse USB açılmaz:
  kök dosya sistemi ayrı bir ext2 bölümündedir ve ISO kipi onu kopyalamaz.
- **Mac'te dd:** aşağıdaki komutlarda `build/banna_live.img` yerine `build/BannaOS.iso`.

Yazılan USB hem UEFI'de (Banna önyükleyicisi) hem eski BIOS'ta (Limine) açılır, Live kipte başlar.
Sanal makinede CD olarak değil **disk olarak** takılmalı (çekirdek CD okuyamaz, kök bulunmaz).

`make live` → `build/banna_live.img` (512 MiB GPT: 64 MiB EFI + ext2 kök, `cmdline=live`).
Live kipte giriş ekranı yoktur: `live` kullanıcısıyla oturum açılır (parola ve sudo parolası:
`live`, hesap diske yazılmaz). Değişiklikler USB'de kalıcıdır.

USB belleğe yazmak (Mac; **diskN'i `diskutil list` ile iki kez doğrula, yanlış disk silinir**):

```sh
make live
diskutil list                       # USB belleğin /dev/diskN adını bul
diskutil unmountDisk /dev/diskN
sudo dd if=build/banna_live.img of=/dev/rdiskN bs=4m
diskutil eject /dev/diskN
```

Bilgisayarda: Secure Boot kapalı olmalı (önyükleyici imzasız), önyükleme menüsünden (F12/F9/Esc,
üreticiye göre) USB'nin UEFI girdisi seçilir.

Live'dan başka bir diske kalıcı kurulum: ikinci USB'yi tak, terminalde `sudo banna-install`
(parola `live`), diski seç ve adını yazarak onayla. Hedef GPT ile bölümlenir (64 MiB EFI FAT32 +
kalan alan ext2), sistem kip ve sahiplikleriyle kopyalanır, `cmdline` boş bir `banna.cfg` yazılır.
Hesaplar kopyalanmaz: kurulan diskten ilk açılışta yönetici hesabı oluşturulur. Yalnızca veri için
ikinci bir USB'yi hazırlamak: `sudo gpt new usb1` (ext2) ya da `sudo gpt new usb1 fat` (her
sistemde okunur).

Bilinen sınırlar (gerçek donanım): UEFI önyükleyicimizle açılınca tek çekirdek kullanılır (diğer
çekirdekleri başlatma şimdilik Limine'ye bağlı); USB hub'ları ve yalnızca EHCI (USB 2) denetleyicili
eski makineler desteklenmez; ekran yalnızca GOP framebuffer'ıdır (hızlandırma yok).

## Açılış ve ekran

Açılışta siyah zemin üzerinde büyük "B" ve yükleme çubuğu gösterilir (initcall ilerlemesi),
ardından masaüstü kendiliğinden açılır. Metin konsoluyla açmak için `limine.conf` içine
`cmdline: console` eklenir. Varsayılan çözünürlük 2560x1440'tır; masaüstü yazıları Inter
(OFL 1.1, `assets/fonts/`) ile kenar yumuşatmalı çizilir, terminal bitmap fontu bu çözünürlükte
2x büyütülür. Font atlasları `scripts/gen_ui_font.swift` ile üretilir.

## Ekran, GPU ve masaüstü

- Arka tampon RAM'dedir; yalnızca hasar gören dikdörtgenler ekrana kopyalanır, framebuffer okunmaz.
- GPU: firmware framebuffer (sabit kip), Bochs/QEMU VGA (`gpu mode 1920x1080`), virtio-gpu
  (kendi tarama kaynağı + açık flush + donanım imleci).
- Compositor olay güdümlüdür: iş yoksa uyur; fare/klavye/konsol/uygulamalar uyandırır; en fazla 60 FPS.
- Pencere içerikleri kendi yüzeylerinde önbelleklenir; opak bir pencerenin örttüğü alanın altı çizilmez.
- İmleç ekran katmanında bindirilir (ya da donanım imleci); hareketi sahneyi yeniden çizdirmez.
- Kullanıcı programları pencere açabilir (`plasma`, `paint`); odaklı uygulamaya klavye ve fare olayları gider.
- Klavye düzeni varsayılan Türkçe Q'dur (AltGr/Option ile `@ # $ { [ ] } \ | ~`); `keymap us` ile ABD düzenine geçilir. Terminal fontu Türkçe harfleri içerir.
- Evrensel hızlandırma (GPU'dan bağımsız): hasar bölgeleri 64 satırlık şeritlere bölünür ve
  `render/N` işçi thread'lerinde tüm çekirdeklerde paralel birleştirilir/ekrana kopyalanır;
  saydam doldurma ve yazı karıştırma SSE2 ile 4 piksel birden yapılır (açılışta öz-sınama).
  `fps bench` tek/çok çekirdek karşılaştırır (QEMU, 4 CPU, 2560x1440: 20.5 → 7.2 ms).
  Donanım 3D hızlandırma (Intel/AMD/NVIDIA çekirdek sürücüleri) yoktur.
- PS/2 IRQ1 ve IRQ12 tek işleyicide toplanır: bayt yalnızca çıkış tamponu doluyken okunur ve durum yazmacındaki AUX bitine göre klavyeye ya da fareye gider.

## CPU

LAPIC + I/O APIC (MADT yönlendirmeleriyle), TSC ile kalibre LAPIC zamanlayıcısı. İkincil
çekirdekler Limine MP ile, kendi UEFI yükleyicimizle açılışta ise çekirdeğin INIT-SIPI-SIPI
trampolini (1 MB altı, gerçek kip → uzun kip; MADT'deki LAPIC kimlikleri) ile başlatılır
 (kendi GDT/TSS, per-CPU GS alanı, syscall MSR'leri, NX/SMEP/SMAP/PAT, SSE).
Zamanlayıcı çok çekirdeklidir: ortak hazır kuyruğu, `on_cpu` ile güvenli bağlam devri,
kayıp uyandırmaya karşı `pending_wake`, boşta çekirdeği IPI ile uyandırma, TLB shootdown,
çekirdek başına yük ölçümü (`cpu`, `ps`). Açılış/kabuk thread'i, donanım IRQ'larının geldiği
0. çekirdeğe sabitlidir.

## Güç ve ses

- ACPI SCI: güç düğmesi temiz kapatma yapar; kapak (`_LID`) kapanınca ekran ışığı kapanır.
- Ekran parlaklığı ACPI `_BCL`/`_BCM` ile: `brightness [0-100]`.
- Intel HWP (Speed Shift) profilleri: `power profile performance|balanced|powersave`;
  AMD'de frekansı firmware yönetir. Sıcaklık: Intel DTS, AMD SMN; 100 °C'de acil kapatma. `power` durumu gösterir.
- Uyku (S3) yoktur: uyanışta ekranı yeniden başlatacak GPU sürücüsü gerekir.
- Intel HD Audio (CORB/RIRB, kodek yolu bulma, 48 kHz 16 bit stereo); yoksa PC hoparlörü.
  `/dev/dsp` ham PCM kabul eder; programlar `beep [Hz] [ms]`, `play <dosya.wav>`; kabuk `volume [0-100]`, `beep`.

## Başlatıcı, dock ve Ayarlar

- Paneldeki **B** logosu (ya da Super/Windows tuşu) uygulama başlatıcısını açar: kayarak ve
  saydamlaşarak belirir; arama kutusu (Türkçe harflerden bağımsız), uygulama ızgarası,
  Oturumu kapat / Yeniden başlat / Kapat. Esc ya da dışarı tıklamak kapatır.
- Dock: sabitlenmiş uygulamalar + açık uygulamalar (ayraçla), açık olanların altında nokta,
  odaklının altında çizgi; üzerine gelince büyür ve adını gösterir. Odaklı uygulamaya tıklamak
  küçültür, küçültülmüşü geri getirir. Pencerelerde kapat ve küçült düğmeleri vardır.
- Simgeler dosyasızdır: işaretli uzaklık alanlarıyla kenar yumuşatmalı çizilir (`gui/icons.rs`).
- Uygulamaların hepsi ring 3'te çalışır ve `apps/` altındadır; başlatıcı ve dock listesi
  `/apps/*/app.conf` dosyalarından gelir (bkz. `apps/README.md`). Uygulamalar masaüstüyle
  `SYS_INFO` (konu 2: ayarlar + canlı bilgi) ve `SYS_DESKTOP_SET` (ayar/eylem) ile konuşur.
- **Ayarlar** (`apps/settings`): Görünüm (koyu/açık tema, 8 vurgu rengi, animasyonlar, gölgeler,
  panel saydamlığı), Duvar kağıdı (8 hazır desen, 8 düz renk, `/usr/share/wallpapers` ve
  `~/Resimler` içindeki BMP/PPM resimleri, karartma), Ekran (canlı çözünürlük değişimi, parlaklık,
  gece ışığı, çok çekirdekli çizim, performans testi), Ses (düzey, sessiz, arayüz sesleri, deneme),
  Güç (HWP profili, sıcaklık, pil, ekranı kapatma süresi, kapak ve güç düğmesi davranışı),
  Klavye ve fare (düzen, imleç hızı, sol el), Tarih ve saat (saat dilimi, 12/24 saat, saniye,
  tarih), Dock ve panel, Ağ (adres bilgileri, DHCP yenile), Kullanıcılar, Hakkında.
- Ayarlar `/etc/banna/desktop.conf`'a yazılır ve açılışta geri yüklenir (dosyada olmayan sistem
  ayarlarına dokunulmaz). Uzun sayfalar fare tekerleği ve ok/PageUp/PageDown ile kayar.
- `terminal -t <başlık> <komut>` komutu terminal penceresinde çalıştırır (Metin Editörü böyle açılır).

## Banna ekosistemi

- **TLS 1.2** (`user/src/tls/`): ECDHE-RSA-AES128-GCM-SHA256, P-256, RSA PKCS#1 imzaları; sertifika
  zinciri gömülü ISRG Root X1'e kadar doğrulanır, ad SAN ile eşleştirilir. `banna::net::get` https
  adreslerini de açar (`http https://...`). `make tls-check` istemciyi ana makinede gerçek sunucuyla dener.
- **Banna Store** (`apps/store`): kataloğu hatch deposunun `index` dosyasından okur; kurma, kaldırma
  ve güncellemeyi yönetici parolasıyla `hatch` yapar. "Mobil uygulamalar" sekmesi ekosistemin
  `/api/banna-store/apps` kataloğunu gösterir.
- **Banna Panel** (`apps/banna-panel`, yalnızca Banna Store'dan: `packages/banna-panel`, `app = banna-panel`):
  masaüstü Banna Panel 4.8.0'ın Rust sürümü. Panel, Bildirimler (Banna Notify, 45 sn yoklama), Banna Mail
  (klasörler, arama, favori/önemli, taşıma, ekler `~/İndirilenler`'e, engellenenler, ekli gönderme), Cihazlar
  (giriş/şifre sıfırlama onayları, onay cihazı), Banna Agent (eşleştirme, jeton, uzaktan kurulum), Güvenlik
  (şifre, hesap silme), Ayarlar (bildirim, yasal metinler, destek), İndirilenler. Oturum çerezleri
  `~/.config/banna/session` (0600), panel ayarları `~/.config/banna/panel.conf`.
- **İlk kurulum** (`apps/setup`): kullanıcının ilk oturumunda terminalden sonra açılır (görünüm, duvar
  kağıdı, Banna hesabı, son adımda `store --install banna-panel`). Bitince `~/.config/banna/welcome` yazılır.
- **Öykünme (UTM/QEMU TCG):** CPUID'de `TCGTCGTCGTCG` görülürse açılış kipi 1920x1080 olur ve kayıtlı tercih
  yoksa animasyonlar kapalı başlar. Boştaki ikincil çekirdeklerin LAPIC zamanlayıcısı durur, PIT tiki uyuyan
  thread'leri yalnızca en yakın uyanma zamanında tarar (boşta QEMU CPU ~%20 → ~%5).
- **hatch** varsayılan olarak ekosistemdeki Banna deposundan okur: `https://ecosys.trybanna.com/hatch/`.
  Depo `banna-repo/scripts/build-hatch.sh` ile üretilir, `publish-to-ecosystem.sh` ile ekosistemin
  `public/hatch` dizinine kopyalanır (BannaEcosystem'in olağan dağıtımıyla yayına girer). Paketler
  `/bin`, `/usr`, `/etc/<paket>` ve `/apps/<paket>` altına kurulabilir.
- **brct** (`tools/brct.rs`): Banna OS için Rust uygulaması derleyicisi; bkz. `apps/README.md`.

## Kullanıcılar ve giriş

Splash'tan sonra grafik giriş ekranı gelir (saat, Türkçe tarih, avatar, Yeniden başlat/Kapat).
İlk açılışta yönetici hesabı oluşturma formu çıkar. Metin konsolunda (`cmdline: console` ya da
ekransız açılış) aynı akış istemle yapılır. Hesaplar `/etc/users`'ta, parolalar PBKDF2-HMAC-SHA256
(10000 tur, 16 bayt tuz) ile saklanır; parola özeti ayrı thread'de hesaplanır, arayüz donmaz.

Her işlemin bir uid'i vardır (başlatandan devralınır). FAT32 izin bitleri taşımadığından erişim
yola göre denetlenir: normal kullanıcı yalnızca kendi ev dizinine (`/home/<ad>`), `/tmp`'ye ve USB
belleklere yazar; `/etc/users` ve başkalarının ev dizinleri okunamaz; root her şeye erişir.
Çekirdek kabuğu (metin konsolu ve kurtarma kipi): `whoami`, `users`, `useradd <ad> [admin]`, `userdel`, `passwd [ad]`, `sudo <komut>`
(yöneticiler, parola sorar), `logout`. Syscall'lar: `getuid`, `username`; erişim reddi `EACCES`.

## USB

xHCI denetleyicisi (`src/drivers/usb/`): BIOS devri, komut/olay halkaları, USB 2/3 port sıfırlama,
aygıt adresleme, kontrol/kesme/bulk aktarımları, STALL kurtarma, sıcak takma (`usbd` thread'i).
Sınıf sürücüleri: HID klavye (boot protokolü → set 1 tarama kodları; Türkçe Q aynen çalışır,
yazılımla tuş tekrarı), HID fare ve tablet (rapor tanımlayıcısı ayrıştırıcısı; mutlak konum),
yığın bellek (BOT + SCSI; `/media/usbN` altına otomatik bağlanır, çıkarılınca ayrılır). Kabuk: `lsusb`,
`mount`, `umount`. USB hub'ları (USB 2/3, iç içe en fazla 5 kat, TT ile düşük/tam hızlı aygıtlar,
sıcak takma) ve EHCI (USB 2 denetleyicisi: async/periyodik QH-qTD, BIOS devri, düşük/tam hızlı
kök aygıtlar eş denetleyiciye bırakılır) desteklenir. Henüz yok: eş zamanlı (isochronous) aktarımlar, MSI,
UHCI/OHCI, kablosuz ağ kartları.

UTM paketinde USB 3.0 açıktır: UTM USB tablet/klavye ekler, imleç yakalamaya gerek kalmaz.

## Ağ

Intel e1000/e1000e (82540EM, 82545EM, 82574L, I217...). DHCP istemcisi (keşif/istek, ARP ile
adres çakışması yoklaması, T1'de yenileme, T2'de yeniden bağlama, kira bitiminde bırakma, bağlantı
gelince INIT-REBOOT), ARP önbelleği ve bekleyen paket kuyruğu, ICMP echo, UDP, DNS (A kaydı, TTL
önbelleği). Bağlantı durumu kesmeyle izlenir. Kabuk: `net`, `ping`, `host`, `dhcp renew|release`, `netstat`.

TCP (`src/net/tcp.rs`, RFC 9293): üçlü el sıkışma, dinleme/kabul, sırasız segment birleştirme,
RFC 6298 RTO, hızlı yeniden iletim + NewReno, go-back-N, sıfır pencere yoklaması, TIME-WAIT, RST.
Kullanıcı programları soketleri fd olarak kullanır (`connect`, `listen`, `accept`, `recv`, `resolve`
syscall'ları; `read`/`write`/`close` ortak). Programlar: `http <url> [dosya]`, `tcpecho [port]`.
QEMU'da yerel indirme ~25 MB/s.

## Bellek yerleşimi

| Bölge | Adres |
| --- | --- |
| Kullanıcı kodu | `0x0000_0000_0040_0000` |
| Kullanıcı mmap | `0x0000_1000_0000_0000` |
| Kullanıcı yığını | `0x0000_7fff_ff00_0000` altında 128 KiB |
| HHDM | Limine verir, genelde `0xffff_8000_0000_0000` |
| Çekirdek heap'i | `0xffff_c000_0000_0000` |
| Çekirdek yığınları | `0xffff_d800_0000_0000` (koruma sayfalı) |
| WC framebuffer / virtio tarama | `0xffff_e000_0000_0000` / `0xffff_e800_0000_0000` |
| Çekirdek imajı | `0xffff_ffff_8000_0000` |

## Derleme ve çalıştırma

```sh
make run        # QEMU (BIOS, ISO) + GPT/ext2 disk + e1000 ağı; seri çıktı terminale gelir
make run-uefi   # aynısı OVMF ile
make disk       # GPT disk imajı (EFI + ext2 kök, build/rootfs'tan); Live USB ile aynı
make iso        # build/BannaOS.iso: Rufus (DD kipi) / balenaEtcher ile USB'ye yazılacak hibrit imaj
make live       # build/banna_live.img (USB'ye yazılacak ham imaj)
make loader     # UEFI önyükleyici
make repo-serve # hatch deposunu üretip 8080'de sunar
make test       # 11 açılış: BIOS, 4 çekirdek + SMEP/SMAP, q35, virtio-gpu, NVMe, USB, kurtarma, batarya, UEFI,
                #   Live USB (kendi önyükleyicimiz + banna-install) ve kurulan USB'den açılış
make check      # fmt + clippy + build
```

Çözünürlük varsayılan olarak 1920x1080'dir: Limine bu kipi ister (`limine.conf`), GPU sürücüsü
de kart izin veriyorsa açılışta bu kipe geçer (`src/drivers/gpu/mod.rs` → `DEFAULT_MODE`).

## UTM (Apple Silicon / Intel Mac)

```sh
make utm        # ISO'yu ve diski BannaOS.utm/Data/ içine kopyalar (VM kapalıyken)
open BannaOS.utm
```

Yapılandırma: x86_64 `pc`, UEFI, 4 çekirdek, 1 GB RAM, standart VGA (BGA ile 1920x1080),
IDE CD + disk, e1000 (Shared ağ), USB 3.0 (xHCI; UTM USB tablet, fare ve klavye ekler) ve yedek
olarak PS/2 denetleyicisi. Tablet mutlak konum verdiği için imleci yakalamak gerekmez. USB
girişinde sorun olursa UTM ayarlarından Input → USB Support → Off ile PS/2'ye dönülebilir.
virtio-gpu UTM'nin SPICE ekranında her flush'ta çok yavaş olduğundan bilerek seçilmedi.
Her yeniden derlemeden sonra `make utm` çalıştırılmalı; UTM yalnızca `Data/` altındaki
dosyaları açar. Diskteki kullanıcı dosyaları korunur, yalnızca `/bin` güncellenir.

Gerekenler: nightly Rust, QEMU, `xorriso`, Git, `dosfstools` (`mkfs.fat`), `mtools`;
ext2 doğrulaması için isteğe bağlı `e2fsprogs` (`e2fsck`, `debugfs`).
Her çekirdek değişikliğinden sonra `make test` çalıştırılmalı.
