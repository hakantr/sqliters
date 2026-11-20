# V6b.180 — Yazılabilir journal_mode sorgusunun hazırlığı

Soğuk yazılabilir ana NamedDisk üzerinde değer vermeyen `PRAGMA [main.]journal_mode` ve EXPLAIN şemayı C ile aynı prepare sınırında yükler. V1, V2 ve reprepare mevcut şema yolu ile çalışır. İlk step öncesi SHARED, rollback NO_LOCK ve WAL/SHM hazırlığı gözlendi. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v178-readwrite-journal-query-prepare-independent/sonuc.md)

428 profil ve 1.284 akışta 672 soğuk C–Rust karşılaştırması fiziksel evreleri eşledi. 640 izli çiftin açma, kilit ve kapanış sırası eş. 1.518 C salt okunur yeniden açılışı, 24 doğal BUSY profili ve 164 WAL kapanış profili doğrulandı. [Strict kayıt](/github/sqliters/target/perf-work/v178-readwrite-journal-query-prepare-independent/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/readwrite-journal-query-prepare-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 557,18 ms taban, 685,53 ms aday. Süre oranı 1,230 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/readwrite-journal-query-prepare-disk-verify/res/gates-validation.txt).
