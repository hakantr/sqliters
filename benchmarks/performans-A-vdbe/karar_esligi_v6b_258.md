# V6b.258 — Görünüm UPDATE tek koşullu tam ve kısmi indeks

Sabit MAIN şemasında ve mevcut açık yazma snapshot’ında tek tam ve tek kısmi indeks, tek basit aralık koşulu için C’nin yapısal uygunluk ayrımını kullanır. Kalıcı sıra ve dosya farkları OFF’ta 108, UTF-16 ilişkisel ON’da 72 kapandı. 330 koruma ve 2.628 C salt okunur açılışı doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v255-view-update-full-partial-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 434,09 ms taban, 632,29 ms aday. Süre oranı 1,457 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-full-partial-disk-verify/res/gates-validation.txt).
