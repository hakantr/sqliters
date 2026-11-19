# V6b.89 — ORDER BY için TRUE ve FALSE adlı sonuç konumları

Tek normal tablo sorgusunda çıplak ORDER BY TRUE/FALSE için C'nin sonuç sütunu önceliği uygulanıyor. İlk açık takma ad, önceki yıldız açılımında aynı kısa sütun adı yoksa seçiliyor. Seçilen sonuç konumu mevcut ordinal yürütme yoluna veriliyor. Nitelikli kaynak adları ve açık sıra numaraları kendi çözümünü koruyor. [C çözümü ve bağımsız ölçüm](/github/sqliters/target/perf-work/truth-order-alias-independent/sonuc.md)

108 hedef akış her kipte ham C eş. 48 ayrı koruma, önceki 108 WHERE ve 114 gerçek sütun akışı, 480 sayısal karşılaştırma ve 18 hazırlama tanısı doğrulandı. C ile salt okunur yeniden açılan 2.565 dosyada SHA-256 eş. Birleşik tam korpusta 387/400 OFF ve 400/400 ON, ham DML özetleri önceki adayla aynı. [Bağımsız kanıt](/github/sqliters/target/perf-work/truth-order-alias-independent/res/proof.sha256) · [Tam korpus](/github/sqliters/target/perf-work/truth-order-final-release-proof/res/proof.sha256)

Birleşik güncel kaynaklarda 108/108 hedef ve 48 koruma yeniden doğrulandı. 780 fiziksel C açılışı eş. [Kesin odak kanıtı](/github/sqliters/target/perf-work/truth-order-exact-focus/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 387/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 299,25 ms taban, 377,59 ms aday. Süre oranı 1,262 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/truth-order-disk-verify/res/gates-validation.txt).
