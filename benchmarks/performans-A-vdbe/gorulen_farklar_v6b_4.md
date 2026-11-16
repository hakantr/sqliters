# V6b.4 — synchronous farkları

D87 (düzeltildi): synchronous ayarı C gibi hazırlamada uygulanır, sorgu sonucu hazırlamada dondurulur; setter adımlamada yeniden uygulanmaz. EXPLAIN hazırlama etkisini yapar. İşlem içindeki setter C hata iletisini verir; TEMP ayarı autocommit içinde etkisizdir. C'nin kabul ettiği 4–6 seviyeleri artık korunur. Sayısal maskenin sarması ve metin varsayılanları C ile aynı.

D88 (düzeltildi): synchronous=EXTRA, xSync'in FULL bayrağını kendiliğinden seçmez. Bu seçimi fullfsync yapar. checkpoint_fullfsync ayrı uygulanır. FULL|DATAONLY birleşimi korunur; günlük silmede sync-dir yalnız EXTRA seviyesinde seçilir. 896 gerçek dosya sistemi senaryosunda C ve Rust çağrıları ve .db baytları aynı.

Kanıt [karar_esligi_v6b_4.md](karar_esligi_v6b_4.md) ve v6b4/res/ altında. Bilinen 15 açık-anahtar testi, DML hata kümeleri, g06 stderr farkı sürüyor. Yetkilendirici/VFS override ve genel hata sonrası pager durumu bu dilimde kapanmadı. V6b ve M4 açık; sırada page_size ve auto_vacuum var. Hız ayarı yok; ileriPlanlama/ değerlendirilmedi.
