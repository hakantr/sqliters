# V6b.452 — WITHOUT ROWID indeks aramalarında fiziksel okuma

C kaynaklı dosyada dört bulunan ve bulunamayan anahtar yolunun satırları, okuma sırası, boyutu ve ofseti iki Rust kipinde pinli C ile eşleşti. NOT INDEXED yolunda 32 okuma ve sayfa sırası eş. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v452-without-rowid-seek-io-rebased-candidate/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v452-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1330 geçti, 1 başarısız, 325 yok sayıldı. ON 1330 geçti, 1 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 300,72 ms taban, 386,35 ms aday. Süre oranı 1,285 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/without-rowid-index-seek-io-disk-verify/res/gates-validation.txt).
