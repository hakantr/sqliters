# V6b.197 — Negatif PERSIST_WAL sorgusunda VFS sonucu

Normal NamedDisk için negatif `PERSIST_WAL` sorgusu C gibi mevcut VFS handle'ına iletiliyor. VFS callback sağlıyorsa sonuç oradan geliyor. Callback bulunmadığında Pager ayarı kullanılıyor. Pozitif setter ve bağlantı kapanışı mevcut yollarını kullanıyor. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v195-persist-wal-file-control-query-independent/sonuc.md)

216 native, 144 fallback ve 36 DETACH yürütmesi doğrulandı. 0/1 setter, -1/-19, bir ana ve bir bağlı dosya, başka aynı-path handle, geçerli C yeniden açılışı ve ham kapanış dosyaları ölçüldü. [Strict kayıt](/github/sqliters/target/perf-work/v195-persist-wal-file-control-query-independent/res/proof.sha256) · [Birleşik tekrar](/github/sqliters/target/perf-work/v197-combined-persist-query.24w0OoKt/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 300,34 ms taban, 378,65 ms aday. Süre oranı 1,261 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/persist-wal-vfs-query-disk-verify/res/gates-validation.txt).
