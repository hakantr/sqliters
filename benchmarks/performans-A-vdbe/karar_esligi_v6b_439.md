# V6b.439 — UTF-16LE ve UTF-16BE tek işçi PMA

64 zorlanmış UTF-16 profili ve 305.818 VFS kaydı C ile eş. Gerçek işçide satır ve reset, açma, okuma ve yazma hata aşamaları doğrulandı. Tüm Rust işçileri birleştirildi. UTF-8 ve named mmap etkileşimi ile 180 olağan FK koşusu korundu. [Bağımsız kanıt](/github/sqliters/target/perf-work/v439-utf16-worker-pma-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1318 geçti, 1 başarısız, 325 yok sayıldı. ON 1316 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 299,29 ms taban, 381,55 ms aday. Süre oranı 1,275 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/utf16-single-worker-pma-disk-verify/res/gates-validation.txt).
