# Banna Search sürüm notları

## 1.1.0
- Sitelere giriş: çerezler artık kalıcı (oturumunuz tarayıcıyı kapatınca gitmez), alan adı, yol, süre, Secure ve HttpOnly kurallarına uyuyor; sayfanın betikleri (document.cookie) aynı çerezleri görüyor.
- Giriş formları: form gönderiminden sonraki yönlendirmeler (POST → 302 → GET) izleniyor ve her adımda çerezler saklanıyor; gönder düğmesinin adı/değeri, formaction ve requestSubmit destekleniyor; istekler Referer ve Origin ile gidiyor.
- Sayfada bul (Ctrl+F): eşleşmeler vurgulanır, Enter / ↑ ↓ ile aralarında gezinilir.
- Yakınlaştırma kısayolları: Ctrl + / Ctrl - / Ctrl 0.
- Adres çubuğu önerileri: yazarken yer imleri ve geçmişten eşleşenler listelenir (↑ ↓ ile seçilir).
- İndirmeler: sayfa olmayan dosyalar (zip, pdf, …) İndirilenler klasörüne kaydedilir.
- Kapatılan sekmeyi yeniden açma (menü), Ctrl+Tab ile sekmeler arası geçiş, Ctrl+D ile yer imi.
- Site uyumluluğu: iç içe tablolar (Hacker News gibi), colspan ve tablo sütun genişlikleri; genişliği yüzdeyle verilen form alanları ve düğmeler (GitHub giriş sayfası); sayfanın CSS'i tarayıcının varsayılan stillerini her zaman geçiyor; düğme içindeki stil metni artık görünmüyor (Google); bağlantıların sözcük aralarındaki alt çizgi.
- JavaScript: `for (--i; ...)` gibi döngüler ayrıştırılıyor.
- Ayarlar → "Çerezler ve site verileri" çerezleri de siler.

## 1.0.2
- Sayfalar daha hızlı işleniyor: yeni bellek ayırıcı, binlerce öğeli sayfalarda stil, yerleşim ve betikleri belirgin biçimde hızlandırıyor (yoğun bellek kullanımında 12 kata kadar).
- Bakım: web motorunda kod temizliği.

## 1.0.1
- Sayfalar daha hızlı açılıyor: aynı sitedeki görseller, stiller ve betikler açık bağlantıdan iniyor; her biri için yeni TCP ve TLS el sıkışması (uzak sitelerde ~0,4 sn) yapılmıyor.
- Düzeltme: Wikipedia gibi çok sayıda CSS değişkeni kullanan sitelerde uygulama birkaç saniye içinde kapanıyordu. CSS değişkenleri artık öğeler arasında paylaşılıyor; bu sayfalarda bellek kullanımı 10 kattan fazla azaldı.
- Görselleri çok olan sayfalar çok daha hızlı yükleniyor: sayfa her görselde baştan yerleştirilmiyor.
- Düzeltme: yüksekliği CSS'te verilmiş konumlu kutulardaki "top: 20%" gibi yüzdeli konumlar yok sayılıyordu; wikipedia.org'daki dil bağlantıları üst üste biniyordu.

## 1.0.0
- Banna OS'un kendi web motoru: HTML5, CSS ve JavaScript.
- Sekmeler, yer imleri, geçmiş ve başlangıç sayfası.
- Geliştirici konsolu (F12).
- Banna hesabıyla (E-Banna adresi ya da E-Kimlik) oturum açma.
