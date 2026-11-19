# V6b.120 — Açık SELECT sırası ve indeks kolasyonu

Tek MAIN rowid kaynağı, açık INDEXED BY ve ORDER BY içeren doğrulanan SELECT'te gerçek C programı sorter kullanmadan bir fiziksel indeks açabilir. KeyInfo istenen yerleşik collation ile eşken saklanan fiziksel indeks tanımlayıcısı farklıysa mevcut ilişkisel sıralama kullanılıyor. Özel collation tanımlayıcısı veya bağlantı callback'i ayrı kapsam. [Kaynak ve yama](/github/sqliters/target/perf-work/v118-mixed-index-order-final-independent/sonuc.md)

Dört ASC/DESC mantıksal fark C ile eşleşti, 12 salt okunur ölçüm ve altı dosya SHA koruması geçti. Birleşik optimize DML OFF 390/400 ve ON 400/400, önceki ham özetle eş. [Exact bağımsız ölçüm](/github/sqliters/target/perf-work/ordered-index-collation-v120-proof/sonuc.md) · [Birleşik korpus](/github/sqliters/target/perf-work/ordered-index-collation-fallback-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 299,19 ms taban, 375,39 ms aday. Süre oranı 1,255 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/ordered-index-collation-fallback-disk-verify/res/gates-validation.txt).
