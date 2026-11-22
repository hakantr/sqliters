# V6b.408 — Üç ikincil indeksli WITHOUT ROWID OR

Tam bileşik birincil anahtar, üç kolda aynı geçici indeksle yönetiliyor. Ortadaki kol önce arıyor ve yalnız yeni anahtarı mevcut arama konumuna ekliyor. 176 UTF-16 sonucu ve 264 tam EXPLAIN planı C ile eşleşti. Önceki iki kollu sonuçlar, C yeniden açma kontrolleri ve uygulama karşılaştırıcısı sınırları korundu. [Bağımsız kanıt](/github/sqliters/target/perf-work/v408-row-case-three-branch-without-rowid-or-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1288 geçti, 1 başarısız, 325 yok sayıldı. ON 1286 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 300,02 ms taban, 384,36 ms aday. Süre oranı 1,281 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-three-branch-without-rowid-or-disk-verify/res/gates-validation.txt).
