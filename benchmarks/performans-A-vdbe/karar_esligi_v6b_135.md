# V6b.135 — Karma indeksli noktasal DML'de kolon yerleşimi

Fiziksel üyelik denetimi, yazıcının kullandığı mantıksal kayıt çözümlemesini paylaşır. Anahtarın gerçek sıra numarası şema metadata'sından alınır. Farklı indeks anahtarı ve INTEGER PRIMARY KEY konumları, örtük rowid ve ALTER öncesi kısa kayıtlar kapsandı. [Bağımsız C/Rust kanıtı](/github/sqliters/target/perf-work/v132-mixed-index-rowid-layouts.h58cbhxv/sonuc.md)

Üç kodlama, beş kolon yerleşimi ve 45 yeni görüntüde 480 commit, 240 C/Rust tam dosya çifti, 60 eski Rust OFF/ON dosya çifti ve 120 rollback başlangıç SHA eş. 1.935 salt okunur yeniden açılış ve 180 gerçek ON geri dönüşü doğrulandı. [Strict fiziksel kanıt](/github/sqliters/target/perf-work/v132-mixed-index-rowid-layouts.h58cbhxv/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/mixed-index-rowid-layouts-final-release-proof/res/proof.sha256)

72 WAL akışında 108 C/Rust ve 18 eski Rust OFF/ON tam ana DB çifti eş. Doğrulanmış nonce alanları ayrıldığında WAL görüntüleri de eş. 198 rollback, 1.584 salt okunur açılış ve 324 gerçek ON geri dönüşü doğrulandı. Ayrı C REINDEX sonrası 12 bağlı API akışında checkpoint sonrası dosya SHA eş. [Bağımsız WAL ölçümü](/github/sqliters/target/perf-work/v135-mixed-index-wal-dml.40QB95KX/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 318,86 ms taban, 394,03 ms aday. Süre oranı 1,236 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/mixed-index-rowid-layouts-disk-verify/res/gates-validation.txt).
