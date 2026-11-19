# V6b.141 — Tek hedefli SQL WAL çıkışında PERSIST_WAL

Normal tek veya nitelikli D/P/T SQL geçişinde, WAL kapanışı VFS PERSIST_WAL kararını C ile aynı amaçta okur. Sonlu journal sınırında WAL aynı inode üzerinde sıfırlanır. Varsayılan sınırla kalan WAL yeniden açılır ve sayfa 1 değişikliği statement bitişinde mevcut WAL'a yazılır. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v139-wal-exit-persist-api-independent/sonuc.md)

1.296 SQL ve 72 olağan kapanış akışında 13.680 ana DB SHA ve 5.130 C salt okunur açılışı eş. 39 C kapsam profili, 2.280 gerçek ON izi ve 15.745 strict kayıt doğrulandı. Eski pager API imzası korunmuş kaynakla fiziksel ölçüm tekrarlandı. [Strict fiziksel kanıt](/github/sqliters/target/perf-work/v139-wal-exit-persist-api-independent/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/wal-exit-persist-platform-release-proof/res/proof.sha256)

Yerleşik non-Unix VFS, C winAccess gibi bildirilen sıfır dosya boyutunu yok sayıyor. 1.746 Linux akışı korundu. Normal VFS boyut modelinde 144 ilk ROW, bitiş, sonraki SELECT ve kapanış farkı kapandı. Native Windows çalıştırması açık. [Platform kanıtı](/github/sqliters/target/perf-work/v145-nonunix-vfs-empty-exists-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 576,77 ms taban, 738,71 ms aday. Süre oranı 1,281 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/wal-exit-persist-disk-verify/res/gates-validation.txt).
