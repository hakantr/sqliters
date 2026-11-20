# V6b.190 — İki hedefli WAL çıkışında busy handler

Ana ve bir bağlı normal yazılabilir disk dosyasının koordineli WAL çıkışında C, ilgili slot için WAL kapanışı hatasını doğrudan döndürüyor. Rust aynı evrede mevcut slot temizliğini yürütüyor ve hatayı busy handler’a göndermiyor. Hazırlık ve daha sonraki sayfa 1 işlemi kendi retry yolunu kullanıyor. [C ve Rust bağımsız karşılaştırma](/github/sqliters/target/perf-work/v189-coordinated-wal-exit-busy-independent/sonuc.md)

324 profil ve 972 yürütmede 540 hedef C çifti eş. 520 eski karşılaştırmada 840 fazla callback kalktı. OFF hedefli yeniden isteğin 42 çiftinde sayfa 1 yazısı ilk ROW’dan C ile aynı bitişe taşındı. Aynı statement V1 ve V2, ayrı yeni V2 ve eş COMMIT sonrası tekrar doğrulandı. [Fiziksel ve API sayımı](/github/sqliters/target/perf-work/v189-coordinated-wal-exit-busy-independent/res/physical-audit.txt) · [Strict kanıt](/github/sqliters/target/perf-work/v189-coordinated-wal-exit-busy-independent/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 303,18 ms taban, 377,73 ms aday. Süre oranı 1,246 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/two-disk-wal-exit-busy-retry-disk-verify/res/gates-validation.txt).
