# V6b.331 — GROUP BY anahtarının sonuç sütununda kullanımı

GROUP BY anahtarını sonuç sütununda kullanma, takma ada bağlı doğrudan COLLATE ifadesinin ayrı değerlendirilmesini koruyacak biçimde doğrulandı. 18 C ve Rust kontrolü, 17 olağan SQL biçimi ve 18 C salt okunur açılışı doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v331-group-key-projection-revised-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1207 geçti, 1 başarısız, 329 yok sayıldı. ON 1204 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 301,33 ms taban, 384,59 ms aday. Süre oranı 1,276 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/group-key-projection-disk-verify/res/gates-validation.txt).
