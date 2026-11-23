# V6b.510 — FILE ve DEFAULT sıralamasında gerçek işçide kayıtlı karşılaştırıcı

Tek ve toplam anahtar iki-PMA profillerinde her kipte 240 C callback dağılımı eş. Geniş sonuç sırası, tekillik ve reset eş. Aynı PMA ataması ölçülen 111/111 Rust koşusunda callback izi eş. Group64 38/38 ve Group65 46/46. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v510-registered-worker-sort-exact/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v510-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1384 geçti, 0 başarısız, 324 yok sayıldı. ON 1384 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 316,58 ms taban, 410,78 ms aday. Süre oranı 1,298 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/registered-pma-worker-comparator-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
