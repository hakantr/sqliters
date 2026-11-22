# V6b.410 — Birincil ve ikincil WITHOUT ROWID OR kolları

Birincil B-tree kolu bileşik anahtarı tablo imlecinden alıyor. İkincil kol aynı anahtar alanlarıyla tabloda konumu doğruluyor. 160 UTF-16 sonucu ve 240 tam EXPLAIN planı C ile eşleşti. İki kolun sırası, aralık genişlikleri, NULL bağlama ve C yeniden açma denetlendi. [Bağımsız kanıt](/github/sqliters/target/perf-work/v410-without-rowid-primary-or-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1290 geçti, 1 başarısız, 325 yok sayıldı. ON 1288 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 307,44 ms taban, 387,66 ms aday. Süre oranı 1,261 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-primary-and-secondary-or-disk-verify/res/gates-validation.txt).
