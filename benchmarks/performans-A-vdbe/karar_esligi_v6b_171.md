# V6b.171 — Salt okunur journal_mode sorgusunun prepare zamanı

C `PRAGMA [main.]journal_mode` değer almadan hazırlandığında gerekirse main şemasını okur. Soğuk salt okunur NamedDisk bağlantısında Rust aynı hazırlık sınırında mevcut şema yükleyicisini kullanır. EXPLAIN ve yeniden hazırlama aynı koşulu kullanır. [Bağımsız C/Rust kanıtı](/github/sqliters/target/perf-work/v166-readonly-journal-query-prepare-independent/sonuc.md)

148 profil ve 444 C/OFF/ON akışında 128 WAL hazırlığının prepare aşamasına geçtiği ölçüldü. 96 fiziksel prepare farkı kapandı. 4.026 ana dosya SHA karşılaştırması ve 531 C yeniden açılışı doğrulandı. [Strict manifest](/github/sqliters/target/perf-work/v166-readonly-journal-query-prepare-independent/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/readonly-journal-query-prepare-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 306,75 ms taban, 382,41 ms aday. Süre oranı 1,247 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/readonly-journal-query-prepare-disk-verify/res/gates-validation.txt).
