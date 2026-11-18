# Banna OS sürüm notları

Sistem güncellemesi (`banna-update`, Banna Store → Güncellemeler) en üstteki bölümün başlığını
ve maddelerini gösterir. Sürüm numarası derleme anında verilir; başlık yayının adıdır.

## hatch diagnose: donanım ve sürücü dökümü
- `hatch diagnose` bilgisayardaki işlemciyi, belleği, PCI ve USB aygıtlarını, diskleri, modelleri ve sürücüleri terminalde listeler.
- `hatch diagnose --send` aydınlatma metnini gösterir; metni kabul ederseniz donanım listeniz Banna hesabınız üzerinden Banna'ya gönderilir. Kabul etmezseniz hiçbir şey gönderilmez.

## Klavye kısayolları
- Klavye: Ctrl + / Ctrl - / Ctrl 0 ve Ctrl+Tab uygulamalara iletiliyor (Banna Search yakınlaştırma ve sekme geçişi).

## Banna Store: dokunuşa yanıt veren düğmeler
- Banna Store'da düğmeler, kartlar ve sekmeler basılınca hafifçe küçülür; işlem fareyi bırakınca yapılır. Yanlışlıkla basarsanız işaretçiyi dışarı çekip bırakarak vazgeçebilirsiniz.
- Arayüz kitaplığında uygulamaların ortak kullanabileceği basma geri bildirimi.

## Sanal masaüstleri ve pencere yaslama
- İki sanal masaüstü: paneldeki 1 · 2 düğmeleri ya da Alt+1 / Alt+2 ile geçin; pencere bulunan masaüstünün altında nokta görünür.
- Pencereyi başka masaüstüne taşıyın: başlığa sağ tıklayıp "Masaüstü 2'ye taşı" ya da Alt+Shift+1 / Alt+Shift+2.
- Pencere yaslama: başlıktan sürükleyip ekranın üst kenarına bırakınca tam ekran, sol ya da sağ kenarına bırakınca ekranın o yarısı. Bırakmadan önce yerleşeceği alan gösterilir.
- Yaslanmış pencereyi başlığından çekince eski boyutuna döner; başlığa sağ tıklayınca büyüt, sola / sağa yasla ve eski boyutuna döndür.
- Bakım ve hız: uygulamaların bellek ayırıcısı yenilendi; küçük nesneler sabit sürede veriliyor (yoğun kullanımda 12 kata kadar hızlı). Kullanıcı kitaplığında kod temizliği.

## Daha hızlı internet
- TCP pencere ölçekleme (RFC 7323) ve 1 MB alma tamponu: uzak sunuculardan indirme hızı bağlantı başına ~0,7 MB/sn'lik tavandan kurtuldu (Banna Store paketleri, sistem güncellemeleri, büyük dosyalar).
- HTTP bağlantıları yeniden kullanılıyor: aynı sunucuya giden istekler TCP ve TLS el sıkışmasını tekrarlamıyor; ardışık istekler 5 kata kadar hızlı.
- Yeni tanılama: "http -n 5 <adres>" aynı adresi art arda isteyip sürelerini gösterir.

## B-Player ve yeni Banna Store
- Yeni uygulama: B-Player. Müzik kitaplığın, ID3 kapakları ve sıvı cam arayüzle MP3 çalar; giriş gerekmez.
- Banna Store'da platform panelleri: Banna OS, Android, iOS, Windows, macOS ve Linux. Bu cihazın paneli "Bu cihaz" olarak açılır.
- Banna Store'da uygulama sayfaları: tanıtım, bu sürümdeki yenilikler, boyut ve sürüm bilgisi.
- Güncellemeler sekmesinde sistem güncellemesi kartı ve sürüm notları.
- Dosyalar uygulaması MP3 dosyalarını B-Player ile açar.
- Düzeltme: bazı pencere yeniden çizimlerinde masaüstünü çökerten hata giderildi.
- Depodaki deneme paketleri (fortune, fortune-tr, merhaba-banna) kaldırıldı; hexdump artık sistemle geliyor.
