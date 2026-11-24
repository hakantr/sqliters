# V6b.538 — İki adlandırılmış kolasyonun işçili ve top-N sıralaması

Her kipte 288 tam sıralama, 864 seri top-N, 216 iki-PMA tekrar ve 576 fallible sözleşme kontrolü eş. Fiziksel PMA ve callback sırası gerçek iş atamasında doğrulandı. [Bağımsız ölçüm](/github/sqliters/target/perf-work/dual-collation-worker-v510-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v538-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1410 geçti, 0 başarısız, 324 yok sayıldı. ON 1410 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 302,84 ms taban, 389,87 ms aday. Süre oranı 1,287 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/dual-named-collation-sort-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
