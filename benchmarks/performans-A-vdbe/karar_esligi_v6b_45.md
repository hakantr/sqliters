# V6b.45 — D149 sıcak journal bölüm başlığı

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. [C dal kapsamı](karar_esligi_v6b_45_dallar.md).

| C yeri ve koşulu | C sonucu | Rust karşılığı ve kanıt |
|---|---|---|
| `pager.c:4091–4113,2889–2899,1347–1349,2460–2466`, journal sync ve boyut | Sync sonrasındaki xFileSize yazımın dayanıklılık sınırıdır. Playback kendi tarama boyutunu okur. Trailer denetimi üçüncü boyutu okur. | `lib.rs:6440–6445,6586–6590,6634–6638` ve `super_journal.rs:136–139` aynı üç çağrıyı yürütür. Her üç xFileSize konumunda ayrı hata ve yeniden sorgulama ölçüldü. |
| `pager.c:2894–2908,1338–1386`, super-journal | Trailer, ilk bölüm başlığından önce okunur. Trailer'daki geçerli super-journal dosyası yoksa işlem commit edilmiştir ve DB sayfaları geri alınmaz. | `lib.rs:6442–6456` önce trailer ve dosyanın varlığını denetler. Var, yok, açık kardeş journal, hatalı magic, checksum ve ad vakaları ile trailer okumalarının hata dönüşleri ölçüldü. |
| `pager.c:1643–1663,2765–2775,2799–2812`, güncel sektör sınırı ve magic | Tam sektör sığmadığında okuma yok. Sığdığında sekiz bayt magic okunur. Uyuşmazlık playback sonudur. | `lib.rs:6266–6289,6501–6507,6594–6600,6607–6612` aynı ilk ve sonraki bölüm sınırlarını kullanır. Eksik başlık, PSOW, PSOW'suz 4096 sektör ve magic uyuşmazlığı ölçüldü. |
| `pager.c:1665–1689,1727–1728`, alan okumaları | nRec, checksum tohumu ve özgün DB sayfa sayısı ayrı dört baytlık xRead çağrılarıyla 8, 12 ve 16 ofsetlerinden okunur. Sektör ve sayfa boyutları yalnız ilk başlıkta 20 ve 24'ten okunur. | `lib.rs:6506–6510,6607–6624` aynı okumaları ilk ve sonraki bölümlerde yaptı. Her alan için IOERR_READ ve IOERR_SHORT_READ, sonraki geometri alanlarında ise silahlandırılan hatanın tetiklenmemesi doğrulandı. 8 ofsetindeki kısmi VFS sayacı da IOERR_SHORT_READ döndürdü. |
| `pager.c:1686–1727,2920–2935,2958–2968`, geometri ve kayıt adedi | Sıfır sayfa boyutu mevcut pager boyutunu devralır. Boyutlar aralık ve ikinin kuvveti denetiminden geçer. İlk geçerli başlık DB boyutunu geri yükler. Sıcak sıfır nRec sıfır kayıt sayılır. `0xffffffff` dosya boyutundan hesaplanır. | `lib.rs:6511–6558` ilk geometriyi ve kayıt adedini uygular. Geçerli ve geçersiz boyutlar, legacy sıfır sayfa boyutu, sıfır nRec, no-sync sayacı ve çok bölümlü journal ölçüldü. |
| `pager.c:2919–2925,2982–2998,3029–3038`, bölüm ve kayıt hatası | Başlık SHORT_READ bir hatadır ve journal sonraki kurtarmaya kalır. Kayıt SHORT_READ başarılı playback sonudur. Başarı durumunda DB sync sonlandırmadan önce gelir. | `lib.rs:6439–6476,6565–6584,6607–6631` başlık hatasını aktarır, kısa kayıtta durur ve başarılı playback'i DB sync ile tamamlar. İki sorgu dönüşü, hata metni ve aradaki dosya özetleri eşit. |

C, Rust OFF ve Rust ON 203 native vakada aynı journal açma, boyut ve xRead izini verdi. Sorgu dönüş kodları, satırlar, hata metinleri ve her sorgu sonrasında DB ve journal dosyalarının SHA-256 değerleri eşit. V6b.44'ün 8 ofsetindeki nRec hata farkı 512, 4096, 8192 ve 65536 bayt sayfaların dördünde kapanıyor. Ham native kayıtlar `/home/hakan/github/sqliters/target/perf-work/v6b49/res/` altında. [Açık farklar](gorulen_farklar_v6b_45.md).

Dayanıklılık sınırı için ayrı sekiz vaka uygulandı. Dört sayfa boyutunda ikinci kayıt dahil tüm journal okunabildiğinde, ilk xFileSize yalnız ilk kaydı kapsıyorsa ana DB'ye yalnız ilk sayfa yazıldı ve sorgu `2` döndürdü. İlk xFileSize iki kaydı da kapsadığında iki eski sayfa yazıldı ve `1` döndü. C, OFF ve ON yazım izleri, sorgu sonuçları ve dosya özetleri sekiz vakanın tamamında eşit. Ham kayıtlar `/home/hakan/github/sqliters/target/perf-work/v6b50-proof/res/` altında.

| Doğrulama kapısı | Sonuç |
|---|---|
| Çalışma alanı, C ikilisi PATH'te | OFF 1144 geçti, 7 başarısız, 337 yok sayıldı. ON 1130 geçti, 21 başarısız, 337 yok sayıldı. Başarısız test kümeleri V6b.44 ile aynı. |
| DML, 6001–6400 | OFF 380/400, ON 400/400. Ham seed JSON'ları V6b.44 ile aynı. |
| Ayrışma bulucu | C ile 245 stdout ve 244 stderr sonucu eşit. V6b.44 ham özeti aynı. |
| Rust 1.100 tabanına hız | Yedi deneme. Toplam medyan taban 297,96 ms, aday 371,74 ms. Aday/taban 1,248. 14 digest eşit. |

Önceki 147 kurtarma vakası, 12 journal imajı ve dört gerçek C çöküş imajı C ile tekrar eşleşti. Dört kapının ham kayıtları, kaynak özeti ve ikili kimlikleri `/home/hakan/github/sqliters/target/perf-work/v6b49_verify/res/` altında.
