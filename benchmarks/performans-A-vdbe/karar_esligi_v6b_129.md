# V6b.129 — Karma kolasyonlu indekslerde güvenli salt okunur erişim

Tek ana şema rowid tablosundaki basit karma yerleşik kolasyon anahtarı için uygun ipucusuz eşitlik ve aralık sorguları ana tabloya yönlendirilir. Gerçek ON programı seçilmiş ikincil kökü açıyorsa aynı SQL ana tablo için yeniden derlenir. OFF mevcut tablo yolunu kullanır. Özgün WHERE, ORDER BY, LIMIT, OFFSET ve bağlanan değerler korunur. [Bağımsız kaynak ve davranış kanıtı](/github/sqliters/target/perf-work/v127-mixed-index-table-scan-independent.UGtym4S2/sonuc.md)

18 görüntüde 324/324 sonuç hem OFF hem ON kipinde C'nin ana tablo referansıyla eş. Önceki 48 OFF ve 20 ON farkı düzeldi. Gerçek ON akışındaki 84 ikincil plan ana tablo için yeniden derlendi. 254 metadata grubu kip başına bayt eş kaldı. [Strict SHA manifesti](/github/sqliters/target/perf-work/v127-mixed-index-table-scan-independent.UGtym4S2/sha256.txt) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/mixed-index-table-scan-final-release-proof/res/proof.sha256)

Exact birleşik release ikilisinde tekrarın ölçüm özeti ve doğal C planı sonucu önceki adayla bayt eş. [Birleşik tekrar manifesti](/github/sqliters/target/perf-work/v129-combined-replay.VHQOQsTq/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 300,53 ms taban, 377,39 ms aday. Süre oranı 1,256 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/mixed-index-table-scan-disk-verify/res/gates-validation.txt).
