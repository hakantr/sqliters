# V6b.84 — Sanal tablo OR ve IN kararları

UTF-8 ON kipinde tek bağlı sanal tabloya uygulanan iki sütun ve tamsayı eşitliğinin OR birleşimi ile iki tamsayı içeren pozitif IN, C hazırlığından gelen programı kullanıyor. OR programındaki ikinci VOpen aynı tablo ve cursor için kabul ediliyor. Filtre, ilerleme ve VOpen işlemleri tek cursor üzerinde kalıyor. VInitIn toplu tarama programları ayrı kalıyor. [Bağımsız kaynak ve vaka raporu](/github/sqliters/target/perf-work/v84-vtab-or-in-independent/sonuc.md)

ON hedefinde 140 akıştan 126'sında tüm ham ölçütler C ile eş. Kalan 14 akışta V1 şema değişimi step sonucu ayrı. 216 OFF akışı tabanla ham eş. 1.080 hedef ve koruma dosyası ile önceki 792 dosya C ile salt okunur yeniden açıldı ve SHA-256 değerleri eşleşti. Bağımsız odak çalışması ilk V81 compiler tabanını kullandı. Güncel dar compiler ile birleştirilmiş adayın 400 vakalık OFF ve ON çıktıları ayrıca doğrulandı. [Odak manifesti](/github/sqliters/target/perf-work/v84-vtab-or-in-independent/res/proof.sha256) · [Birleşik korpus](/github/sqliters/target/perf-work/vtab-or-in-final-release-proof/res/proof.sha256)

Dar compiler tabanında normal odak 140 hedef akışı ve aynı 126 ham C eşliğini yeniden verdi. 72 koruma ve 1.060 fiziksel C açılışı eş. [Kesin taban ve adayın ham kanıtı](/github/sqliters/target/perf-work/v84-vtab-exact-focus/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 383/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 302,82 ms taban, 381,05 ms aday. Süre oranı 1,258 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/vtab-or-in-disk-verify/res/gates-validation.txt).
