# V6b.100 — Görünüm MAX eşitliği ve ek indeksler

Görünüm UPDATE scalar MAX eşitliğinde, tek ilgili bileşik başlangıç anahtarı ve ek ilgisiz düz indeksler aday kapsamına alınır. Gerçek tarama indeksini mevcut planlayıcı seçer. Aynı başlangıç anahtarını paylaşan indeksler ile partial, expression, stat1 ve özel kolasyon kararları ayrı kalır. [C kaynak ve bağımsız ölçüm](/github/sqliters/target/perf-work/view-max-multi-index-independent/sonuc.md)

186 yeni native grupta her API için ham OFF 12/93'ten 75/93'e, ON 39/93'ten 81/93'e çıktı. Ana tablo 93/93. Önceki 816 koruma ölçüldü ve ilgisiz ikinci indeks vakası C'ye eşitlendi. 93 C EXPLAIN/EQP ve 69 gerçek Rust plan/cache izi ayrık ölçüldü. Birleşik optimize derleme önkoşusunda DML OFF 388/400'den 389/400'e çıktı. 6314 trigger sırası C eş. ON 400/400 ve önceki ham özetler aynı. [Bağımsız SHA](/github/sqliters/target/perf-work/view-max-multi-index-independent/sha256.txt) · [Tam korpus](/github/sqliters/target/perf-work/view-max-multi-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 389/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 299,30 ms taban, 375,54 ms aday. Süre oranı 1,255 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-max-multi-disk-verify/res/gates-validation.txt).
