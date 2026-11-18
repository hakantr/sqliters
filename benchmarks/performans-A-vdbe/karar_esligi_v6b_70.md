# V6b.70 — Özel geçici veritabanı dosyasının açılış zamanı

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. Başlangıç V6b.69 `5708afe4f881fb7dceeed451cbafd67effc1545b`.

C VFS gözlemcisi, iki özel geçici DB'yi boş adla açtı. Boş okuma, `BEGIN IMMEDIATE`, küçük yazma ve COMMIT boyunca TEMP_DB açılış sayısı sıfırdı. Bu sırada LOCKSTATE sonucu 12 ve çıktı argümanı ilk değerinde kaldı. İki sayfalık cache altında büyük yazma ilk dosyayı açtı ve LOCKSTATE 0 döndü. İkinci DB aynı geçişi kendi yazmasında yaptı. Her biri kendi ilk satırını ve 31 satırlık toplamı korudu, iki integrity_check de `ok` verdi. [C ham iz](/home/hakan/github/sqliters/target/perf-work/private-temp-lifecycle-proof/res/probe-c.txt).

Rust pager testinin küçük yazma ve COMMIT beklentisi bu karara göre güncellendi. Ayrı cache spill, iki farklı OS dosyası, NO_LOCK tanısı, her pager'da kendi sayfa içeriği ve kapanışta dosyaların kaldırılması denetleniyor. Doğrudan OsVfs yolunda küçük COMMIT sonrası okunabilir veri ve açılmamış dosya tanısı da doğrulanıyor.

| C kararı | Ölçüm |
|---|---|
| `pager.c:5984–6036` | Altı normal TEMP yazma başlangıcı, her biri WRITER_LOCKED. VFS açılışı yok. |
| `pager.c:2030–2034` | Sekiz COMMIT kontrolünün dördünde dosya henüz açık değil. Dört açık TEMP kontrolü dirty oranına ulaştı. |
| `pager.c:4663–4738,4493–4502` | 116 stress ve 118 sayfa listesi yazımı. Dosya kapalı dalı iki kez alındı ve iki TEMP_DB xOpen oldu. |

Açık dosyanın tekrar kullanımı 116 sayfa listesinde gözlendi. Stres ölçümünde prior ERROR, SPILLFLAG_OFF/ROLLBACK, WAL yolu ve VFS hata sonuçları alınmadı. Begin için EXCLUSIVE ve pager upgrade hata yönleri bu profilde alınmadı. [C dal sayaçları](/home/hakan/github/sqliters/target/perf-work/private-temp-lifecycle-proof/res/decisions.txt) · [Kaynak ve kanıt özetleri](/home/hakan/github/sqliters/target/perf-work/private-temp-lifecycle-proof/res/sha256.txt).

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1170 geçti, 5 başarısız, 337 yok sayıldı. ON 1157 geçti, 18 başarısız, 337 yok sayıldı. Özel geçici DB testi iki kipte geçti. Yeni başarısızlık yok. |
| DML ve ayrışma bulucu | OFF 380/400, ON 400/400. Finder stdout 245/245 ve stderr 244/245. Ham özetler önceki adımla aynı. |
| Kurtarma | 147/147, dosya imajı 12/12, gerçek C çöküşü 4/4. |
| Performans tabanı | 304,19 ms taban, 375,46 ms aday. Süre oranı 1,234, 14 digest eşit. |

[Kapı doğrulaması](/home/hakan/github/sqliters/target/perf-work/private-temp-lifecycle-verify/res/gates-validation.txt).
