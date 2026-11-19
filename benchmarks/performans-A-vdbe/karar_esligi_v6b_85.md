# V6b.85 — STRICT ve NOT NULL kontrol sırası

Ortak yazma yolunda normal NOT NULL kontrolü, NOT NULL REPLACE varsayılanıyla generated ifadenin yeniden hesaplanması ve ikinci NOT NULL geçişi C kod üretim sırasını izliyor. STRICT generated ifadeden önce normal sütunları, VIRTUAL hesaplandığında ilgili register'ı ve fiziksel kayıt kararında STORED sütunları denetliyor. Legacy UPSERT önerisi normal NOT NULL kontrolünü STRICT kayıt kontrolünden önce yürütüyor. [C kaynak kararı ve bağımsız deney](/github/sqliters/target/perf-work/strict-constraint-order/sonuc.md)

Üç veritabanı kodlaması ve iki hazırlama API'si ile 324 grup ölçüldü. OFF V2 ham eşliği 84/162'den 159/162'ye, V1 15/162'den 39/162'ye çıktı. Doğru ON'da V2 110/162'den 160/162'ye, V1 24/162'den 40/162'ye çıktı. Fiziksel eşlik OFF 132/162 ve ON 142/162'den her API ve kipte 162/162'ye çıktı. FAIL önceki satırı ve işlem açıklığını korudu. ROLLBACK işlemin önceki yazımlarını kaldırdı. Dört mevcut generated UPSERT ve trigger kontrolü geçti. [Ham ölçümler](/github/sqliters/target/perf-work/strict-constraint-order/res/proof.sha256)

Birleşik optimize adayda DML OFF 383/400’den 387/400’e çıktı. 6082, 6185, 6242 ve 6330 C ile eşleşti. Diğer 13 ilk farklı gözlem korundu. ON 400/400. [Tam korpus kanıtı](/github/sqliters/target/perf-work/strict-write-final-release-proof/res/proof.sha256)

İlk bağımsız sürücü ON için yanlış ortam anahtarını kullanmıştı. Doğru `SQLITERS_VDBE_EXEC=1` ile aynı kaynak ve ikililer yeniden ölçüldü. Önceki OFF kayıtları birebir doğrulandı. [Düzeltilmiş ON ölçümü](/github/sqliters/target/perf-work/strict-constraint-order/on-duzeltme.md)

İlk paralel workspace koşusunda OFF ve ON aynı C referans manifestini yazdı. OFF readback kontrolü yeni başarısızlık verdi ve bu koşu kabulden çıkarıldı. Aynı kaynak ve ikiliyle kiplere sıralı uygulanan koşuda referans kontrolü iki kipte geçti. [İlk paralel deneme ve doğrulayıcı](/github/sqliters/target/perf-work/strict-write-disk-verify/res/parallel-attempt/validator.txt)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 387/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 296,97 ms taban, 372,87 ms aday. Süre oranı 1,256 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/strict-write-disk-verify/res/gates-validation.txt).
