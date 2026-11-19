# V6b.145 — Kalıcı WAL yeniden açılmadan önce SHARED kilidi

Varsayılan boyut sınırında retained WAL yeniden açılmadan önce, tek hedefli normal D/P/T WAL çıkışı C gibi SHARED kilidine iner. C'nin ara kilit çağrısının sonucunu kullanmama davranışı izlenir. Gerçek VFS kilit bilgisi mevcut başarı koşuluyla güncellenir. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v143-persist-reattach-lock-independent/sonuc.md)

1.296 normal akışta 144 ilk ROW ve aynı 144 kapanış kilit farkı giderildi. 216 canlı C salt okunur eş akışında mevcut snapshot ve sonraki işlemde yeni sayfa 1 eş. 13.584 DB SHA, 6.792 C açılış çifti ve 30 izole C kapsam profili doğrulandı. [26.652 strict kayıt](/github/sqliters/target/perf-work/v143-persist-reattach-lock-independent/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/persist-reattach-lock-platform-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 542,52 ms taban, 699,66 ms aday. Süre oranı 1,290 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/persist-reattach-lock-disk-verify/res/gates-validation.txt).
