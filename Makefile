# Banna OS build, boot-image and QEMU helpers.
# Override tools/settings when necessary, for example:
#   make run QEMU_MEM=1G QEMU_SMP=4

CARGO       ?= cargo
QEMU        ?= qemu-system-x86_64
XORRISO     ?= xorriso
GIT         ?= git
FILE        ?= file

TARGET      := x86_64-banna_os
KERNEL      := target/$(TARGET)/debug/banna_os
RELEASE_KERNEL := target/$(TARGET)/release/banna_os
BUILD_DIR   := build
ISO_ROOT    := $(BUILD_DIR)/iso_root
ISO         := $(BUILD_DIR)/banna_os.iso
LIMINE_DIR  := $(BUILD_DIR)/limine
LIMINE_TAG  ?= v8.x-binary

USER_DIR    := user
USER_OUT    := $(USER_DIR)/target/x86_64-banna_user/release
USER_BINS   := hello fsck banna-update ls cat sysinfo fputest http tcpecho sh mkdir rm touch wc grep edit sudo hatch \
               lsblk mount umount gpt chmod chown mv cp banna-install mkfs beep play banna-agent banna-crash banna-run hexdump
# Masaüstü uygulamaları: kaynak apps/<kimlik>/, kurulum /apps/<kimlik>/ (app.conf + program).
# packages/<kimlik> paketi olan uygulamalar diske konmaz, Banna Store'dan kurulur.
APPS        := $(filter-out $(notdir $(wildcard packages/*)),$(notdir $(patsubst %/app.conf,%,$(wildcard apps/*/app.conf))))
DISK        := $(BUILD_DIR)/disk.img
DISK_MB     ?= 512
MKFS_FAT    ?= mkfs.fat

QEMU_MEM    ?= 256M
QEMU_SMP    ?= 1
QEMU_FLAGS  ?= -m $(QEMU_MEM) -smp $(QEMU_SMP) -no-reboot -no-shutdown -boot d \
               -drive file=$(DISK),format=raw,if=ide,index=0 \
               -netdev user,id=net0 -device e1000,netdev=net0
QEMU_SHARE  := $(shell brew --prefix qemu 2>/dev/null)/share/qemu
OVMF_CODE   := $(BUILD_DIR)/OVMF_CODE.fd
OVMF_VARS   := $(BUILD_DIR)/OVMF_VARS.fd
OVMF_FLAGS  := -drive if=pflash,format=raw,readonly=on,file=$(OVMF_CODE) -drive if=pflash,format=raw,file=$(OVMF_VARS)

.DEFAULT_GOAL := help

.PHONY: brct brct-install tls-check system-update iso modules
.PHONY: help all build release check fmt fmt-check clippy inspect tools limine image user disk rootfs esp loader live repo repo-serve utm run run-uefi run-headless run-debug test gdb clean distclean

help: ## Kullanılabilir hedefleri gösterir.
	@awk 'BEGIN {FS = ":.*##"}; /^[a-zA-Z_-]+:.*##/ {printf "  %-15s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

all: build ## Çekirdeği derler.

build: ## Debug çekirdeğini özel bare-metal hedef için derler.
	$(CARGO) build

release: ## Optimize edilmiş çekirdeği derler.
	$(CARGO) build --release

fmt: ## Kaynak kodu biçimlendirir.
	$(CARGO) fmt

fmt-check: ## Biçimlendirmeyi değiştirmeden denetler.
	$(CARGO) fmt --check

clippy: ## Çekirdek kodu için lint çalıştırır.
	$(CARGO) clippy --target $(TARGET).json -- -D warnings

check: fmt-check clippy build ## Biçim, lint ve derleme kontrollerini yapar.

inspect: build ## Oluşan çekirdeğin ELF bilgisini gösterir.
	$(FILE) $(KERNEL)

tools: ## Gerekli yerel araçların kurulu olduğunu denetler.
	@command -v $(CARGO) >/dev/null || { echo "Eksik araç: $(CARGO)"; exit 1; }
	@command -v $(QEMU) >/dev/null || { echo "Eksik araç: $(QEMU)"; exit 1; }
	@command -v $(XORRISO) >/dev/null || { echo "Eksik araç: $(XORRISO)"; exit 1; }
	@command -v $(GIT) >/dev/null || { echo "Eksik araç: $(GIT)"; exit 1; }
	@echo "Araçlar hazır."

$(LIMINE_DIR)/limine:
	@mkdir -p $(BUILD_DIR)
	$(GIT) clone --depth 1 --branch $(LIMINE_TAG) https://github.com/limine-bootloader/limine.git $(LIMINE_DIR)
	$(MAKE) -C $(LIMINE_DIR)

limine: $(LIMINE_DIR)/limine ## Sabitlenmiş Limine önyükleyicisini indirir ve derler.

$(ISO): build tools $(LIMINE_DIR)/limine limine.conf
	@rm -rf $(ISO_ROOT)
	mkdir -p $(ISO_ROOT)/boot/limine $(ISO_ROOT)/EFI/BOOT
	cp $(KERNEL) $(ISO_ROOT)/boot/banna_os
	cp limine.conf $(ISO_ROOT)/boot/limine/limine.conf
	cp $(LIMINE_DIR)/limine-bios.sys $(LIMINE_DIR)/limine-bios-cd.bin $(LIMINE_DIR)/limine-uefi-cd.bin $(ISO_ROOT)/boot/limine/
	cp $(LIMINE_DIR)/BOOTX64.EFI $(ISO_ROOT)/EFI/BOOT/
	$(XORRISO) -as mkisofs -b boot/limine/limine-bios-cd.bin -no-emul-boot -boot-load-size 4 -boot-info-table \
		--efi-boot boot/limine/limine-uefi-cd.bin -efi-boot-part --efi-boot-image --protective-msdos-label \
		$(ISO_ROOT) -o $(ISO)
	$(LIMINE_DIR)/limine bios-install $(ISO)

image: $(ISO) ## BIOS ve UEFI uyumlu Limine ISO üretir.

user: ## Kullanıcı programlarını (user/) derler.
	cd $(USER_DIR) && $(CARGO) build --release

LOADER_DIR  := loader
OBJCOPY     ?= $(shell rustc --print sysroot)/lib/rustlib/$(shell rustc -vV | sed -n 's/host: //p')/bin/llvm-objcopy
LOADER_EFI  := $(LOADER_DIR)/target/x86_64-unknown-uefi/release/banna-loader.efi
MKIMAGE     := $(BUILD_DIR)/mkimage
ROOTFS      := $(BUILD_DIR)/rootfs
ESP_IMG     := $(BUILD_DIR)/esp.img
ESP_MB      ?= 64
LIVE_CMDLINE ?= live

loader: ## Banna UEFI önyükleyicisini (BOOTX64.EFI) derler.
	cd $(LOADER_DIR) && $(CARGO) build --release

BRCT        := $(BUILD_DIR)/brct

$(BRCT): tools/brct.rs
	@mkdir -p $(BUILD_DIR)
	BANNA_SDK=$(CURDIR) rustc --edition 2024 -O tools/brct.rs -o $(BRCT)

brct: $(BRCT) ## brct (Banna Rust Compatibility Tool) aracını derler: build/brct.

tls-check: ## TLS istemcisini ana makinede test vektörleri ve ecosys.trybanna.com ile dener.
	@mkdir -p $(BUILD_DIR)
	rustc --edition 2024 -O tools/tls_check.rs -o $(BUILD_DIR)/tls_check
	$(BUILD_DIR)/tls_check

brct-install: $(BRCT) ## brct'yi ~/.cargo/bin altına kurar (PATH'te olur).
	@mkdir -p $(HOME)/.cargo/bin && cp $(BRCT) $(HOME)/.cargo/bin/brct
	@echo "brct kuruldu: $(HOME)/.cargo/bin/brct"

$(MKIMAGE): tools/mkimage.rs src/fs/ext2.rs
	@mkdir -p $(BUILD_DIR)
	rustc --edition 2024 -O tools/mkimage.rs -o $(MKIMAGE)

WALLPAPER   := $(BUILD_DIR)/wallpapers/kuzey-isiklari.bmp

$(WALLPAPER): scripts/gen_wallpaper.py
	@mkdir -p $(dir $@)
	python3 scripts/gen_wallpaper.py $@ 1600 900

MODULES     := rtl8139
MODULES_OUT := modules/target/x86_64-banna_kmod/release

modules: ## Yüklenebilir çekirdek modüllerini (modules/) derler.
	cd modules && $(CARGO) build --release

rootfs: user modules $(WALLPAPER) ## Kök dosya sistemi ağacını build/rootfs altında hazırlar.
	@rm -rf $(ROOTFS)
	@mkdir -p $(ROOTFS)/bin $(ROOTFS)/boot $(ROOTFS)/dev $(ROOTFS)/docs $(ROOTFS)/etc \
		$(ROOTFS)/home $(ROOTFS)/media $(ROOTFS)/mnt $(ROOTFS)/root $(ROOTFS)/tmp \
		$(ROOTFS)/usr/share $(ROOTFS)/var
	@for bin in $(USER_BINS); do cp $(USER_OUT)/$$bin $(ROOTFS)/bin/$$bin; done
	@for app in $(APPS); do mkdir -p $(ROOTFS)/apps/$$app && cp apps/$$app/app.conf $(ROOTFS)/apps/$$app/; \
		if [ -f $(USER_OUT)/$$app ]; then cp $(USER_OUT)/$$app $(ROOTFS)/apps/$$app/$$app; fi; \
		for png in apps/$$app/*.png; do [ -f $$png ] && cp $$png $(ROOTFS)/apps/$$app/; done; true; done
	@mv $(ROOTFS)/bin/mkfs $(ROOTFS)/bin/mkfs.ext2 && cp $(ROOTFS)/bin/mkfs.ext2 $(ROOTFS)/bin/mkfs.fat
	@printf 'Merhaba Banna\n' > $(ROOTFS)/docs/HELLO.TXT
	@printf 'banna\n' > $(ROOTFS)/etc/hostname
	@mkdir -p $(ROOTFS)/etc/banna && printf '%s\n' "$(SYSTEM_VERSION)" > $(ROOTFS)/etc/banna/version
	@python3 scripts/driver_versions.py $(ROOTFS)/etc/banna/drivers
	@mkdir -p $(ROOTFS)/lib/modules && for m in $(MODULES); do cp $(MODULES_OUT)/$$m $(ROOTFS)/lib/modules/$$m.bko; done
	@if [ -d firmware ]; then mkdir -p $(ROOTFS)/lib/firmware && cp -R firmware/. $(ROOTFS)/lib/firmware/; fi
	@mkdir -p $(ROOTFS)/usr/share/wallpapers && cp $(WALLPAPER) $(ROOTFS)/usr/share/wallpapers/
	@mkdir -p $(ROOTFS)/usr/share/brct && cp assets/brct/*.png $(ROOTFS)/usr/share/brct/
	@mkdir -p $(ROOTFS)/usr/share/hwdata && cp assets/hwdata/*.ids $(ROOTFS)/usr/share/hwdata/
	@echo "rootfs: $(ROOTFS)"

esp: image loader ## EFI sistem bölümünü (FAT32) hazırlar: yükleyici, çekirdek, ayarlar.
	@rm -f $(ESP_IMG)
	@dd if=/dev/zero of=$(ESP_IMG) bs=1m count=$(ESP_MB) 2>/dev/null
	@$(MKFS_FAT) -F 32 -s 1 -n BANNAEFI $(ESP_IMG) >/dev/null
	@mmd -i $(ESP_IMG) ::/EFI ::/EFI/BOOT ::/boot ::/boot/limine
	@mcopy -i $(ESP_IMG) $(LOADER_EFI) ::/EFI/BOOT/BOOTX64.EFI
	@$(OBJCOPY) --strip-debug $(KERNEL) $(BUILD_DIR)/banna_os.stripped
	@mcopy -i $(ESP_IMG) $(BUILD_DIR)/banna_os.stripped ::/boot/banna_os
	@printf '# Banna OS önyükleme ayarları\ntimeout=3\ncmdline=$(LIVE_CMDLINE)\n' > $(BUILD_DIR)/banna.cfg
	@mcopy -i $(ESP_IMG) $(BUILD_DIR)/banna.cfg ::/boot/banna.cfg
	@printf 'timeout: 3\n\n/Banna OS\n    protocol: limine\n    kernel_path: boot():/boot/banna_os\n    cmdline: $(LIVE_CMDLINE)\n' > $(BUILD_DIR)/limine-disk.conf
	@mcopy -i $(ESP_IMG) $(BUILD_DIR)/limine-disk.conf ::/boot/limine/limine.conf
	@mcopy -i $(ESP_IMG) $(LIMINE_DIR)/limine-bios.sys ::/boot/limine/limine-bios.sys
	@echo "esp: $(ESP_IMG)"

disk: rootfs esp $(MKIMAGE) ## GPT disk imajı: EFI bölümü + ext2 kök. Live USB'ye de yazılır.
	$(MKIMAGE) disk $(DISK) $(DISK_MB) $(ESP_IMG) $(ROOTFS)
	@$(LIMINE_DIR)/limine bios-install $(DISK) >/dev/null 2>&1 || echo "disk: limine bios-install başarısız (yalnızca UEFI)"
	@echo "disk: $(DISK) ($(USER_BINS); apps: $(APPS))"

live: disk ## Live USB imajı: build/banna_live.img (dd ile USB belleğe yazılır, bkz. README).
	cp $(DISK) $(BUILD_DIR)/banna_live.img
	@echo "live: $(BUILD_DIR)/banna_live.img — USB'ye yazmak için README'deki 'Live USB' bölümüne bak"

HYBRID_ISO  := $(BUILD_DIR)/BannaOS.iso

iso: disk ## USB'ye yazılacak hibrit imaj: build/BannaOS.iso (balenaEtcher / Rufus "DD" kipi / dd).
	bash scripts/build_hybrid_iso.sh $(DISK) $(HYBRID_ISO) $(BUILD_DIR)/banna_os.stripped $(LIMINE_DIR)

REPO_DIR    := $(BUILD_DIR)/repo
REPO_PORT   ?= 8080

repo: user ## packages/ tanımlarından hatch deposunu build/repo altına üretir.
	python3 scripts/build_repo.py $(REPO_DIR)

system-update: disk ## Sistem güncellemesini (çekirdek, yükleyici, programlar) build/repo/system altına imzalı üretir.
	python3 scripts/build_system.py $(REPO_DIR)/system

repo-serve: repo ## Depoyu HTTP ile sunar (misafirde varsayılan kaynak: http://@gateway:8080/).
	python3 -m http.server $(REPO_PORT) --bind 0.0.0.0 --directory $(REPO_DIR)

# Kurulu sistemin sürümü (/etc/banna/version); banna-update bununla karşılaştırır.
SYSTEM_VERSION ?= $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)-$(shell date +%Y%m%d%H%M)

UTM_BUNDLE  := BannaOS.utm
UTM_DATA    := $(UTM_BUNDLE)/Data

utm: image disk ## ISO ve diski UTM paketine (BannaOS.utm/Data) kopyalar; VM kapalıyken çalıştır.
	@if pgrep -f "$(UTM_DATA)" >/dev/null; then echo "BannaOS.utm şu an çalışıyor; önce VM'i kapat."; exit 1; fi
	@mkdir -p $(UTM_DATA)
	@if [ ! -f $(UTM_BUNDLE)/config.plist ]; then cp tools/utm/config.plist $(UTM_BUNDLE)/config.plist; \
		echo "utm: config.plist eksikti, tools/utm/config.plist'ten geri yüklendi"; fi
	cp $(ISO) $(UTM_DATA)/banna_os.iso
	@if [ ! -f $(UTM_DATA)/disk.img ]; then cp $(DISK) $(UTM_DATA)/disk.img; \
	elif offset=$$($(MKIMAGE) esp-offset $(UTM_DATA)/disk.img 2>/dev/null); then \
		$(MKIMAGE) update $(UTM_DATA)/disk.img $(ROOTFS)/bin && \
		for app in $(APPS); do $(MKIMAGE) update $(UTM_DATA)/disk.img $(ROOTFS)/apps/$$app /apps/$$app || exit 1; done && \
		mcopy -o -i $(UTM_DATA)/disk.img@@$$offset $(BUILD_DIR)/banna_os.stripped ::/boot/banna_os && \
		mcopy -o -i $(UTM_DATA)/disk.img@@$$offset $(LOADER_EFI) ::/EFI/BOOT/BOOTX64.EFI; \
	else mv $(UTM_DATA)/disk.img $(UTM_DATA)/disk-eski-fat32.img && cp $(DISK) $(UTM_DATA)/disk.img && \
		echo "utm: eski FAT32 disk GPT/ext2 sistem diskiyle değiştirildi (yedek: $(UTM_DATA)/disk-eski-fat32.img)"; fi
	@echo "utm: $(UTM_BUNDLE) hazır (UTM'de aç ya da çift tıkla)"

run: image disk ## ISO'yu QEMU penceresinde (BIOS) başlatır; seri çıktı terminale gelir.
	$(QEMU) $(QEMU_FLAGS) -serial stdio -cdrom $(ISO)

$(OVMF_CODE):
	@mkdir -p $(BUILD_DIR)
	cp $(QEMU_SHARE)/edk2-x86_64-code.fd $(OVMF_CODE)
	cp $(QEMU_SHARE)/edk2-i386-vars.fd $(OVMF_VARS)

run-uefi: image disk $(OVMF_CODE) ## ISO'yu UEFI (OVMF) firmware ile başlatır.
	$(QEMU) $(QEMU_FLAGS) $(OVMF_FLAGS) -serial stdio -cdrom $(ISO)

run-headless: image disk ## ISO'yu penceresiz başlatır; yalnızca seri çıktı.
	$(QEMU) $(QEMU_FLAGS) -nographic -cdrom $(ISO)

test: image disk repo ## BIOS ve UEFI açılışında self-test işaretlerini denetler.
	./scripts/regression_test.sh

run-debug: image disk ## QEMU'yu GDB bekleyerek başlatır (başka terminalde `make gdb`).
	$(QEMU) $(QEMU_FLAGS) -s -S -cdrom $(ISO)

gdb: build ## GDB'yi çekirdek sembolleriyle açar; `make run-debug` sonrası bağlanır.
	gdb $(KERNEL) -ex 'target remote :1234'

clean: ## Cargo ve oluşturulan ISO dosyalarını temizler.
	$(CARGO) clean
	cd $(USER_DIR) && $(CARGO) clean
	rm -rf $(ISO_ROOT) $(ISO)

distclean: clean ## İndirilen Limine dahil tüm oluşturulan dosyaları temizler.
	rm -rf $(BUILD_DIR)
