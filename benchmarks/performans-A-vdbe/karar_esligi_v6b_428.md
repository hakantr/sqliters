# V6b.428 — Anonim sıralama ve geçici indeks dosyalarının ömrü

İki dosya türü ve iki umask altında C ile aynı 0600, sıfır dizin bağı ve temiz süreç sonlandırması doğrulandı. 576 LIMIT/OFFSET profili yeniden eşleşti; MMAP_SIZE bildirimi hatası sıralamayı kesmedi. [Bağımsız kanıt](/github/sqliters/target/perf-work/v428-anonymous-sorter-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1307 geçti, 1 başarısız, 325 yok sayıldı. ON 1305 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 298,89 ms taban, 381,53 ms aday. Süre oranı 1,277 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/pma-anonymous-unix-temp-file-disk-verify/res/gates-validation.txt).
