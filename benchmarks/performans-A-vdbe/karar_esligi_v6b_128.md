# V6b.128 — Açık TEMP şemasında WAL geçişi

Açık TEMP şemasının özel geçici pagerı WAL sınırını geçemediği için disk koordinasyonu ön kontrolünde katılımcı sayılmaz. Nitelemesiz geçişte gerçek NamedDisk katılımcıları C ile aynı ilk ROW ve bitiş sınırlarını izler. TEMP FILE PERSIST ve TRUNCATE istekleri pagerın normal kip seçimine ulaşır. TEMP MEMORY kendi kipini korur. [Native C ve Rust kanıtı](/github/sqliters/target/perf-work/v126-open-temp-journal-mode-independent/sonuc.md)

384 erken ROW akışı ve 288 TEMP FILE kip gözlemi düzeldi. 1.026 normal statement akışında 14.688 ana DB SHA, 5.508 salt okunur C açılışı ve 3.168 TEMP metadata gözlemi doğrulandı. 10.732 strict SHA kaydı ve 14 izole C gcov profili korundu. [Bağımsız manifest](/github/sqliters/target/perf-work/v126-open-temp-journal-mode-independent/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/open-temp-journal-mode-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 299,88 ms taban, 379,11 ms aday. Süre oranı 1,264 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/open-temp-journal-mode-disk-verify/res/gates-validation.txt).
