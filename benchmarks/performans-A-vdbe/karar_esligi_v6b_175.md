# V6b.175 — Salt okunur journal_mode setter hazırlığı

Soğuk salt okunur NamedDisk main bağlantısında C, `PRAGMA [main.]journal_mode=...` çalıştırılmadan önce şemayı prepare aşamasında okur. Rust önceki sorgu yolunu setter ve EXPLAIN için uygular. Şema hazır olduğunda mevcut akış yürür. [Bağımsız C/Rust karar kaydı](/github/sqliters/target/perf-work/v173-readonly-setter-prepare-independent/sonuc.md)

178 profil ve 534 akışta 216 hazırlık sınırı ve 136 fiziksel WAL/SHM farkı kapandı. 696 C yeniden açılışı ve 5.190 ana dosya SHA üçlüsü eş. V1 ilk step ve reset sözleşmesi, 42 native V2 BUSY ve 74 IOERR_LOCK sonucu korundu. [Strict manifest](/github/sqliters/target/perf-work/v173-readonly-setter-prepare-independent/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/readonly-journal-setter-prepare-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 306,25 ms taban, 383,19 ms aday. Süre oranı 1,251 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/readonly-journal-setter-prepare-disk-verify/res/gates-validation.txt).
