# V6b.423 — Tamponlu seviye 0 PMA dosyası

Bağlantı VFS dosyasındaki seviye 0 PMA biçimi, 8.530 C yazma ve okuma ile eş ofset ve boyut gösterdi. İki Rust kipinde açma hatası karşılaştırmadan önce döndü. [Bağımsız kanıt](/github/sqliters/target/perf-work/v423-pma-level0-file-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1303 geçti, 1 başarısız, 325 yok sayıldı. ON 1301 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 298,04 ms taban, 377,66 ms aday. Süre oranı 1,267 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/pma-level0-vfs-file-disk-verify/res/gates-validation.txt).
