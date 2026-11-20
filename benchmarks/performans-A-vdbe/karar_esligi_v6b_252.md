# V6b.252 — STRFTIME biçim sonunda eşleşmemiş yüzde

STRFTIME biçimi tek eşleşmemiş yüzde ile sona erdiğinde C gibi SQL NULL döner. 1.944 biçim girdisinde 399 fark kapandı. Kaçırılmış yüzde çiftleri, geçerli 741 önceki çıktı ve 1.536 tarih kontrolü eş. Üç fiziksel dosya ve 15 C salt okunur açılışı doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v252-strftime-dangling-percent-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 243/245, determinism 244/245. Açık L4 ve L6 kayıtları doğrulandı. |
| Performans tabanı | 299,52 ms taban, 378,56 ms aday. Süre oranı 1,264 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/strftime-dangling-percent-disk-verify/res/gates-validation.txt).
