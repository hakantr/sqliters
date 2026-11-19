# V6b.139 — Karma indeksli rowid aralık DML'i

Doğrudan rowid aralığıyla seçilen normal UPDATE ve DELETE, uygun karma yerleşik indeksli tablonun fiziksel karşılaştırıcısını aynı Btree yazma görüntüsünden seçer. Açık alt ve üst sınırlar, BETWEEN, tek taraflı karşılaştırmalar ve bağlanan değerler C ile aynı ileri satır sırasını izler. [Bağımsız kaynak ve davranış kanıtı](/github/sqliters/target/perf-work/v136-mixed-index-rowid-range.A5M6A8UR/sonuc.md)

81 DELETE/WAL akışında 216 yazıcı ve rollback kontrolü, 108 C/Rust ve 27 eski Rust OFF/ON tam DB çifti, 90 doğrulanmış nonce ayrımlı WAL çifti ve 1.512 salt okunur açılış eş. Çok sayfalı, bağlı V2 ve negatif rowid korumaları geçti. [Strict fiziksel kanıt](/github/sqliters/target/perf-work/v136-mixed-index-rowid-range.A5M6A8UR/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/mixed-index-rowid-range-final-release-proof/res/proof.sha256)

Exact birleşik release ikilisiyle normal ve çok sayfalı aralık matrisleri aynı sonuçları verdi. [8.761 strict birleşik tekrar kaydı](/github/sqliters/target/perf-work/v139-combined-replay.w3VdnfdZ/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 300,45 ms taban, 379,36 ms aday. Süre oranı 1,263 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/mixed-index-rowid-range-disk-verify/res/gates-validation.txt).
