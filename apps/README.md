# Banna OS uygulamaları

Her masaüstü uygulaması kendi klasöründe durur ve **ring 3'te** (kullanıcı alanında) çalışır;
çekirdekte uygulama kodu yoktur. Çekirdekteki compositor yalnızca pencereleri birleştirir,
paneli, dock'u ve başlatıcıyı çizer.

```
apps/<kimlik>/
    app.conf     tanım dosyası (zorunlu)
    main.rs      program (komut satırı programını terminalde açan uygulamalarda yok)
```

Kurulu sistemde aynı düzen `/apps/<kimlik>/` altındadır: `app.conf` ve `<kimlik>` programı.
Başlatıcı ve dock listeyi açılışta ve başlatıcı her açıldığında `/apps` klasöründen okur;
kabuk `/bin`'de bulamadığı komutu `/apps/<ad>/<ad>` olarak da arar.

## app.conf

```
name=Ayarlar                 # başlatıcı ve dock'taki ad
description=Sistemi özelleştir
icon=settings                # terminal settings monitor editor paint plasma calculator about ...
# icon=image:icon.png        # ya da uygulama klasöründeki PNG (saydam arka planlı)
# builder=brct               # brct ekler: Brct uygulamasında listelenir, Store'da rozet
title=Ayarlar                # pencere başlığı (açık pencereyi uygulamayla eşler)
dock=2                       # dock'a sabitleme sırası (yoksa sabitlenmez)
order=2                      # başlatıcıdaki sıra
single=1                     # tek pencere: yeniden açmak var olanı öne getirir
exec=edit                    # varsayılan /apps/<kimlik>/<kimlik>
terminal=1                   # komut satırı programı: terminal penceresinde çalışır
requires=/bin/calc           # bu dosya yoksa uygulama gösterilmez
```

## Repo dışında uygulama yazmak (brct)

`brct` (Banna Rust Compatibility Tool) ana makinede yazılan Rust projelerini Banna OS için
derler; `cargo build --release` yerine `brct <yol> --release` kullanılır:

```
make brct-install                 # brct'yi ~/.cargo/bin altına kurar
brct new merhaba                  # masaüstü uygulaması (--cli: komut satırı programı)
brct merhaba --release            # → merhaba/target/banna/release/merhaba ve merhaba.app/
brct package merhaba              # → packages/merhaba (Banna Store / hatch paketi)
```

Giriş noktası `banna::main!(main)`, API'ler `use banna::prelude::*`. Örnek: `examples/merhaba-banna`.

## Yeni uygulama eklemek

1. `apps/<kimlik>/main.rs` ve `apps/<kimlik>/app.conf` oluştur.
2. `user/Cargo.toml` içine `[[bin]] name = "<kimlik>"`, `path = "../apps/<kimlik>/main.rs"` ekle.
3. `make disk` uygulamayı `/apps/<kimlik>/` altına kurar.
4. Diske değil Banna Store'a konacaksa `packages/<kimlik>/package.ini` içine `app = <kimlik>` yaz:
   `make repo` uygulamayı pakete koyar, `make disk` atlar.

Programlar `banna` kütüphanesini kullanır:

- `banna::gui::Window`: pencere, olaylar (fare, tekerlek, klavye, kapatma), `canvas()`
- `banna::gfx`, `banna::icons`: masaüstüyle aynı çizim kodu ve Inter yazı tipi
- `banna::ui`: kart, satır, anahtar, kaydırıcı, seçim düğmesi, renk örneği
- `banna::theme`: masaüstü teması (koyu/açık, vurgu rengi)
- `banna::desktop`: ayarları ve sistem bilgisini okuma (`query`), ayar değiştirme (`set`)
- `banna::net`: http ve https istekleri (`get`, `request`); `banna::tls`: TLS 1.2 istemcisi
- `banna::ecosystem`: Banna Ekosistemi API'si (E-Banna oturumu, kataloglar); `banna::json`
- `banna::task`: arka planda komut çalıştırıp çıktısını okuma

## Uygulamalar

| Klasör | Uygulama |
|---|---|
| `terminal` | Terminal (pty + `/bin/sh`; `terminal -t <başlık> <komut>`) |
| `settings` | Ayarlar (11 sayfa) |
| `store` | Banna Store: platform panelleri (Banna OS, Android, iOS, Windows, macOS, Linux); açılışta bu cihazın paneli. Banna OS'ta depodan kurma/kaldırma/güncelleme, diğerlerinde indirme bağlantıları |
| `b-player` | B-Player: MP3 çalar (sıvı cam, ID3 kapakları, giriş gerekmez; `banna::mp3` çözücüsü) |
| `setup` | Kurulum Sihirbazı (ilk oturumda açılır) |
| `banna-panel` | Banna Panel: E-Banna hesabı, bildirimler, Banna Mail, cihazlar, Banna Agent — **yalnızca Banna Store'dan** (`packages/banna-panel`) |
| `monitor` | Sistem İzleyici |
| `about` | Banna OS Hakkında |
| `paint` | Paint |
| `plasma` | Plazma (grafik gösterisi) |
| `editor` | Metin Editörü (`edit`, terminalde) |
| `calculator` | Hesap Makinesi (`calc`, `hatch install calc` ile) |
