# V6b.249 — Rakamsız saniye kesri

Saat metninde saniyeden sonraki nokta en az bir kesir rakamı gerektirir. Sabit C ile 648 sorgu karşılaştırmasındaki 36 fark ve beş ek odak farkı kapandı. Önceki 612 sonuç bayt eş. UTF8 DELETE ve UTF16LE WAL kalıcı dosyaları C ile bayt eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v249-empty-time-fraction-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 242/245, determinism 244/245. Açık L1, L4 ve L6 kayıtları doğrulandı. |
| Performans tabanı | 298,94 ms taban, 381,39 ms aday. Süre oranı 1,276 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/empty-time-fraction-disk-verify/res/gates-validation.txt).
