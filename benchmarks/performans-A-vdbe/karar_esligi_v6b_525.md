# V6b.525 — WITHOUT ROWID fiziksel PK ile adlandırılmış FILE sıralaması

Her kipte 1.152 named ve 192 stock profil geçti. Eş görev dağılımında 1.440 writer/reader/callback karşılaştırması C eş. Geriye dönük stock ve fallible sözleşmesi geçti. [Bağımsız ölçüm](/github/sqliters/target/perf-work/named-without-rowid-worker-v510-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v525-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1397 geçti, 0 başarısız, 324 yok sayıldı. ON 1397 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 557,38 ms taban, 729,52 ms aday. Süre oranı 1,309 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/named-without-rowid-worker-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
