# Sürücü Yöneticisi sürüm notları

## 1.0.2
- Daha hızlı ve akıcı: yeni bellek ayırıcı küçük nesneleri sabit sürede veriyor (yoğun kullanımda 12 kata kadar hızlı).

## 1.0.1
- Bağlantılar daha hızlı: aynı sunucuya giden istekler açık bağlantıyı yeniden kullanıyor (her istekte yeni TCP ve TLS el sıkışması yapılmıyor).

## 1.0.0
- Aygıtlar ve onları süren sürücüler tek listede.
- Çekirdek modüllerini yükleme.
- Sürücü güncellemelerini denetleyip yeniden başlatınca kurulmak üzere hazırlama.
