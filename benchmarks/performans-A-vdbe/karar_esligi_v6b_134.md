# V6b.134 — Karma indeksli noktasal DML'de fiziksel sıra

Uygun iki kolonlu normal MAIN rowid tablo, karma yerleşik kolasyonlu tek doğrudan TEXT indeks ve dar INSERT/UPDATE/DELETE için iki Rust kipi mevcut ilişkisel yazıcıyı kullanır. İlk değişiklikten önce aynı Btree yazma görüntüsünde rowid, gerçek değer, indeks üyeliği ve fiziksel sıra karşılaştırılır. Dış kural geçerliyse C düzeni, yalnız iç geçerliyse eski Rust düzeni kullanılır. [Bağımsız kaynak ve davranış kanıtı](/github/sqliters/target/perf-work/v132-mixed-index-rowid-dml-prototype.OnqsCXT9/sonuc.md)

384 commit, 76.512 fiziksel kayıt, 192 C/Rust tam dosya çifti, 48 eski Rust OFF/ON dosya çifti ve 96 rollback başlangıç SHA doğrulandı. Normal C REINDEX sonrasında aynı bağlı deyim yeni görüntüde C son dosya SHA değerine ulaştı. [Strict fiziksel kanıt](/github/sqliters/target/perf-work/v132-mixed-index-rowid-dml-prototype.OnqsCXT9/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/mixed-index-rowid-write-final-release-proof/res/proof.sha256)

48 WAL akışında 72 C/Rust ve 12 eski Rust OFF/ON tam ana DB çifti eş. Doğrulanmış nonce alanları ayrıldığında WAL görüntüleri de eş. 132 rollback, 1.056 salt okunur açılış ve 216 gerçek ON geri dönüşü doğrulandı. [Exact V134 WAL ölçümü](/github/sqliters/target/perf-work/v135-mixed-index-wal-dml.40QB95KX/sonuc.md)

Birleşik release ikilisiyle DML, nullable ve eski okuma kapsamları tekrar ölçüldü. [Birleşik tekrar 10.211 strict kayıt](/github/sqliters/target/perf-work/v134-combined-replay.5JVlrJHS/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 578,21 ms taban, 739,92 ms aday. Süre oranı 1,280 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/mixed-index-rowid-write-disk-verify/res/gates-validation.txt).
