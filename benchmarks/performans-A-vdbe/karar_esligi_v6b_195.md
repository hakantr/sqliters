# V6b.195 — Journal mode adlarının C önek kuralı

SQLite C, `PRAGMA journal_mode` değerini ASCII harf duyarsız önekle belirliyor. Hazırlama ve yürütme artık bu ortak kararı kullanıyor. `Wa` WAL, boş tırnaklı değer DELETE, tanınmayan ve sonuna boşluk eklenen değer mevcut kip sorgusu olarak değerlendirilir. [C kararı](/github/sqliters/target/perf-work/v187-wal-exit-busy-retry-independent/c/sqlite3.c:146050) · [Fiziksel doğrulama](/github/sqliters/target/perf-work/v194-journal-mode-names.BcfrpI51/sonuc.md)

672 tek ve 72 bağlı dosya C–Rust çiftinde SQL ve yeniden açılış eş. Tek dosyalı 252 eski SQL sonucu ve bağlı dosyalı 30 eski çift düzeldi. 636 tek dosya ham kapanışı ve 72 bağlı dosya çifti bayt eş. [Ham sayımlar](/github/sqliters/target/perf-work/v194-journal-mode-names.BcfrpI51/res/audit.txt) · [Exact kanıt](/github/sqliters/target/perf-work/v194-journal-mode-names.BcfrpI51/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 301,45 ms taban, 378,02 ms aday. Süre oranı 1,254 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/journal-mode-name-prefix-disk-verify/res/gates-validation.txt).
