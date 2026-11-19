# V6b.170 — İşlem içi journal_mode V1 dış tanısı

C, etkin okuyucu veya açık transaction içindeki WAL sınırı değişikliğini reddeder. V1 dış step mevcut bağlantı tanısını, V2 ayrıntılı statement hatasını bildirir. Rust legacy dış tanısı gözlenen genel Error sınıfını da kapsıyor. [Bağımsız C/Rust native kanıt](/github/sqliters/target/perf-work/v169-journal-mode-v1-transaction.RaKCJuwz/sonuc.md)

24 native vaka grubunda 24 V1 mesaj farkı kapandı ve 24 V2 karşılaştırması eş. Etkin okuyucu devamı, transaction'ın sonlandırılması ve ardından normal kip değişimi doğrulandı. 48 tam DB imajı ve 72 C açılışı eş. [Strict kanıt](/github/sqliters/target/perf-work/v169-journal-mode-v1-transaction.RaKCJuwz/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/journal-mode-v1-transaction-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 302,78 ms taban, 379,26 ms aday. Süre oranı 1,253 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/journal-mode-v1-transaction-disk-verify/res/gates-validation.txt).
