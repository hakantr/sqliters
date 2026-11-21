# V6b.296 — Tcl test kabuğunda sabit saat

Ayrı test host’ları için Tcl bağlı tamsayısı disk VFS saatine yansıtıldı. 77 Tcl linked-int ve 43 harness kontrolü geçti. İki kipte 21 tarih testi daha aynı beklentiyi verdi. [Bağımsız kanıt](/github/sqliters/target/perf-work/v290-date-tcl-fixed-clock-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 572,24 ms taban, 737,87 ms aday. Süre oranı 1,289 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/date-tcl-fixed-clock-disk-verify/res/gates-validation.txt).
