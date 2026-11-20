# V6b.187 — Bir bağlı dosyayla boş main WAL girişi

Boş yazılabilir ana NamedDisk, en fazla bir bağlı NamedDisk ile nitelemesiz WAL girişinde C ile aynı statement bitişine katılır. İki gerçek katılımcı olduğunda kilit ve terminal sayfa 1 yazma sıraları ayrı korunur. Doğal BUSY sonrasında hedef kipteki bağlı pager'ın geçici sıfır sayfa sayımı ek yazıya dönüşmez. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v185-attached-empty-main-wal-staging-independent/sonuc.md)

711 profil ve 2.133 akışta 1.188 fiziksel C–Rust hedef çifti doğrulandı. 60 doğal BUSY profilinde aynı fiziksel sonuç, yeniden hazırlama ve C yeniden açılışı geçti. [Strict manifest](/github/sqliters/target/perf-work/v185-attached-empty-main-wal-staging-independent/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/attached-empty-main-wal-staging-final-release-proof/res/proof.sha256)

Exact birleşik native ikili 60 doğal BUSY ve 24 canlı C okur profilinde aynı fiziksel sonuç, tanı ve C yeniden açılışını verdi. [Birleşik tekrar](/github/sqliters/target/perf-work/v187-combined-stage-replay.dzZ5Zity/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 309,36 ms taban, 389,23 ms aday. Süre oranı 1,258 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/attached-empty-main-wal-staging-disk-verify/res/gates-validation.txt).
