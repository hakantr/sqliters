# V6b.168 — Karma ve olağan indeksi aynı işlemde koru

Tek karma ve ikinci olağan doğrudan köklü MAIN rowid tabloda, nokta veya aralık yazısı ve VALUES eklemesi ilk mutasyondan önce iki fiziksel kökü aynı işlem görüntüsünde doğrular. Integrity_check karma kökü C karşılaştırıcısıyla denetler. [Bağımsız Rust 1.100 ve C kanıtı](/github/sqliters/target/perf-work/v168-rust-1.100-reproof.MEdwVf32/sonuc.md)

288 native yazıcı, 576 rollback ve 864 C/OFF/ON açılış, 360 normalize WAL, 144 C/Rust ve 36 eski OFF/ON tam DB çifti eş. Birleşik CLI ile altı taze C dosyasında RTrim yazımı ve iki kök bütünlük çıktısı bayt eş. [13.469 strict kayıt](/github/sqliters/target/perf-work/v168-rust-1.100-reproof.MEdwVf32/sha256.txt) · [Birleşik metadata](/github/sqliters/target/perf-work/v168-combined-index-xinfo.P5pFEuSU/proof.sha256) · [Birleşik DML](/github/sqliters/target/perf-work/mixed-plus-ordinary-index-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 308,39 ms taban, 386,52 ms aday. Süre oranı 1,253 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/mixed-plus-ordinary-index-disk-verify/res/gates-validation.txt).
