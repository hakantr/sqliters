# V6b.420 — Uygulama kolasyonlu MEMORY LIMIT ve OFFSET

Altı sağlıklı DELETE/WAL dosyasında 372 profil ve iki Rust kipinde toplam 21.732 adım C ile eşleşti. 300 önceki karşılaştırıcı farkı kapandı. Özgün geri çağrı hatası, genişletilmiş kod, ofset, reset, yeniden hazırlama ve yetkilendirici denetlendi. [Bağımsız kanıt](/github/sqliters/target/perf-work/v420-custom-topn-step-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1300 geçti, 1 başarısız, 325 yok sayıldı. ON 1298 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 311,64 ms taban, 397,31 ms aday. Süre oranı 1,275 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/custom-memory-limit-sort-step-disk-verify/res/gates-validation.txt).
