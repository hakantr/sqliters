# V6b.334 — GROUP BY sonucunda MIN/MAX girdisinin temsili

MIN/MAX ile döndürülen grup sütunu C’nin seçtiği girdi değerini taşıyor. Önceki 130 düzeltme korundu, 30 INTEGER/REAL sınır farkı kapandı. İlk adayda bozulan integer kernel testi düzeltilen adayda geçti. On bir odaklı C ve Rust kontrolü doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v334-group-minmax-representative-revised-candidate/sonuc.md)

Üç kodlamada dokuz WAL okur/yazar akışı ve dokuz C yeniden açılışı geçti. Açık okurun snapshot’ı, commit sonrası satır türü ve ana dosya SHA’sı eş. [WAL okur snapshot kanıtı](/github/sqliters/target/perf-work/v334-wal-reader-exact-proof.hCbOY24k/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1210 geçti, 1 başarısız, 329 yok sayıldı. ON 1207 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 300,86 ms taban, 384,12 ms aday. Süre oranı 1,277 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/group-minmax-representative-disk-verify/res/gates-validation.txt).
