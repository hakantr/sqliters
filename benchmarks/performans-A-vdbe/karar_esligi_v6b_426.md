# V6b.426 — İkinci PMA dosyasının tamponlu birleştirme I/O sırası

1.577.890 okuma ve yazmanın ofsetleri ile boyutları ve 24 açma ve kapama tam eşleşti. 2.052 adım ile 16.033 karşılaştırma eş, 12 ara PMA callback hatasında I/O durdu. [Bağımsız kanıt](/github/sqliters/target/perf-work/v426-pma-intermediate-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1306 geçti, 1 başarısız, 325 yok sayıldı. ON 1304 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 299,30 ms taban, 380,26 ms aday. Süre oranı 1,271 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/pma-intermediate-vfs-file-disk-verify/res/gates-validation.txt).
