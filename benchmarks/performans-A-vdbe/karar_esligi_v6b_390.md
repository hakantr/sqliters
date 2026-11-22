# V6b.390 — Fiziksel indeks üzerinden satır CASE gruplaması

Exact V6b.389 tabanıyla 684 gözlemde fiziksel NOCASE indeksinin beslediği 32 gruplanmış sonuç C ile eşleşti. Önceki 558 CASE kaydı ve bağlantı kolasyonu korumaları eş. C fixture 10/10, Rust rowvalue8 22/22 ve delete2 1/1 iki kipte. [Bağımsız kanıt](/github/sqliters/target/perf-work/v390-row-case-streaming-group-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1271 geçti, 1 başarısız, 325 yok sayıldı. ON 1268 geçti, 4 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 302,18 ms taban, 381,96 ms aday. Süre oranı 1,264 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-streaming-group-disk-verify/res/gates-validation.txt).
