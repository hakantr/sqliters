# V6b.254 — Finder S10 WITHOUT ROWID DELETE

Sağlıklı MAIN WITHOUT ROWID tablosunda örtük tam taramalı DELETE için kanıtlanan tek tam covering indeks, mevcut C uyumlu tarama ve silme sırasını kullanır. Kanonik ve ortak 15.000 kayıtlı başlangıç dosyasında Rust OFF/ON ile C L0. 165 bağımsız C salt okunur açılışı, 12 hedef ve 21 koruma profili ile iki tam yeni Finder koşusu geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v251-finder-s10-covering-delete-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 317,06 ms taban, 480,14 ms aday. Süre oranı 1,514 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/finder-s10-covering-delete-disk-verify/res/gates-validation.txt).
