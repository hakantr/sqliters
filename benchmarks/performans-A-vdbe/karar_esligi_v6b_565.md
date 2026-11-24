# V6b.565 — Altı kaynaklı INNER JOIN’de dört ila altı şema

222 hedef fark kapandı ve 114 doğru kontrol korundu. 336/336 C yaşam döngüsü, 168/168 gerçek Tcl V1, 7.128 C yeniden açılışı ve önceki 1.356 ham kayıt eş. [Bağımsız ölçüm](/github/sqliters/target/perf-work/six-schema-inner-reprepare-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v565-with-v548-correction-proof/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1439 geçti, 0 başarısız, 319 yok sayıldı. ON 1439 geçti, 0 başarısız, 319 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 299,44 ms taban, 392,12 ms aday. Süre oranı 1,310 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/six-schema-inner-join-binding-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
