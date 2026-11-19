# V6b.112 — C foreign key raporlama kimliği

`foreign_key_list`, son bildirilen kısıttan başlayarak C kimliğini döndürüyor. Bileşik anahtarın sütun `seq` değeri ve action metadata aynı C tablosuna göre ölçüldü. `foreign_key_check.fkid` aynı raporlama numarasını hesaplıyor. [C kaynak ve metadata kanıtı](/github/sqliters/target/perf-work/foreign-key-list-id-independent/sonuc.md)

Üç kodlamada önceki 36 metadata grubunun 72 farkı kapandı. 27 tablo değerli ve boş şema salt okunur kontrolü, 18 fiziksel dosya ve C açılışı eş. Birleşik optimize DML önceki ham özetle eş ve OFF 390/400, ON 400/400. [Odak manifesti](/github/sqliters/target/perf-work/foreign-key-list-id-independent/sha256.txt) · [Birleşik korpus](/github/sqliters/target/perf-work/foreign-key-list-id-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 299,28 ms taban, 374,31 ms aday. Süre oranı 1,251 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/foreign-key-list-id-disk-verify/res/gates-validation.txt).
