# Üçüncü taraf bileşenler

Banna OS'un kodu bu depoda yazılmıştır; aşağıdaki bileşenler başka kaynaklardan gelir ya da
onlardan türetilmiştir.

| Bileşen | Nerede | Lisans |
|---|---|---|
| [Inter](https://rsms.me/inter/) yazı tipi (Rasmus Andersson) | `assets/fonts/`; arayüzün ve Banna Search'ün glif atlasları bu yazı tipinden üretilir (`scripts/gen_ui_font.swift`) | SIL Open Font License 1.1 — `assets/fonts/OFL.txt` |
| [minimp3](https://github.com/lieff/minimp3) (lieff) | `user/src/mp3/`: Layer III kod çözme yolunun Rust'a aktarımı; `tables.rs` minimp3'ün tablolarından betikle çıkarıldı | CC0 1.0 (kamu malı) |
| [Limine](https://github.com/limine-bootloader/limine) önyükleyicisi | Derleme sırasında indirilir (`make limine`); depoda yoktur | BSD-2-Clause |
| [PCI ID Repository](https://pci-ids.ucw.cz/) ve [USB ID Repository](http://www.linux-usb.org/usb-ids.html) | `assets/hwdata/`: `hatch diagnose`'un aygıt adları (`scripts/update_hwdata.py` ile sadeleştirilir) | GPL-2.0-or-later ya da BSD-3-Clause (çift lisans) |
| Kök sertifikalar | `user/src/tls/roots/*.der`: TLS istemcisinin güvendiği genel sertifika otoritelerinin açık sertifikaları | Kamuya açık veriler |
