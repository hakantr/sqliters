# V6b.147 — Tek tırnaklı COLLATE adı

Ayrıştırıcı, C'nin COLLATE adı kabul ettiği dört konumda tek tırnaklı diziyi mevcut ad ve dequote akışıyla okur. Sağlıklı C dosyaları ve doğrudan SQL sözdizimi iki Rust kipinde de açılır. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v143-single-quoted-collation.LFS73k7H/sonuc.md)

12 sağlıklı C veritabanı iki kipte salt okunur açıldı. Her kipte 84/84 SELECT ve 192/204 metadata sonucu C ile eş. 11 ayrıştırıcı testi, 726 önceki metadata grubu ve 108 açılış koruması geçti. [2.766 strict kayıt](/github/sqliters/target/perf-work/v143-single-quoted-collation.LFS73k7H/sha256.txt) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/single-quoted-collation-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 301,70 ms taban, 381,06 ms aday. Süre oranı 1,263 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/single-quoted-collation-disk-verify/res/gates-validation.txt).
