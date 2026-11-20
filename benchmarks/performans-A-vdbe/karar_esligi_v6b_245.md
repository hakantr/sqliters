# V6b.245 — Görünüm UPDATE iki tam indeks

Sabit şemalı doğrudan rowid görünüm UPDATE’inde iki tam nonunique BINARY indeks arasından tek uyumlu literal aralığı mevcut aralık yoluyla izler. 644 sıra ve dosya farkı kapandı. 204 kapsam koruması ve 336 önceki native kontrol eş. 5.100 native ve 5.100 C açılışı, 34.934 strict doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v244-view-update-two-full-index-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 242/245, determinism 244/245. Açık L1, L4 ve L6 kayıtları doğrulandı. |
| Performans tabanı | 302,40 ms taban, 378,11 ms aday. Süre oranı 1,250 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-two-full-index-disk-verify/res/gates-validation.txt).
