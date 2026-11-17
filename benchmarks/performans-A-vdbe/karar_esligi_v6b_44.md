# V6b.44 — D147/D148 sıcak journal playback

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. [C dal kapsamı](karar_esligi_v6b_44_dallar.md).

| C yeri ve koşulu | C sonucu | Rust karşılığı ve kanıt |
|---|---|---|
| `pager.c:2374–2399`, sıcak kayıt | Sayfa numarası 4, sayfa verisi page_size, checksum 4 bayt okunur. Sınır dışındaki sayfa için checksum okunmaz. | `read_hot_record_part` C sırasındaki okuma boyutları ve ofsetlerini kullanır. 512, 4096, 8192 ve 65536 bayt sayfalarda ilk kaydın izi eşit. |
| `pager.c:2390,2404,2978`, sıcak playback'te pDone NULL | Sıcak playback geçerli kayıtları sırayla uygular. Aynı sayfanın sonraki geçerli kaydı önceki içeriğin üzerine yazılır. | `play_hot_journal` sıcak kayıtları tekilleştirmez. Dört sayfa boyutunda iki geçerli kayıttan sonuncusunun sayfa baytları kaldı. |
| `pager.c:2982–2999`, kısa kayıt veya başka okuma hatası | SHORT_READ playback'i bitirir. Önceki geçerli kayıtlar kalır. IOERR_READ gibi başka hata aktarılır ve journal bir sonraki okumaya bırakılır. | SHORT_READ ve IOERR_READ kayıt sayfası okumasına enjekte edildi. Her iki sorgu sonucu, journal varlığı, dönüş kodu ve kalıcı dosya eşit. Kesilmiş ve checksum'ı bozulmuş ikinci kayıt imajı da denetlendi. |
| `pager.c:2726–2757,2959`, ana DB boyutu | Eşit uzunlukta xTruncate yok. Büyük dosya kısaltılır. En az bir tam sayfa eksikse son sayfaya sıfır yazılır. | Playback özgün boyutu xFileSize ile karşılaştırır. Son sayfa yazımı ve kısaltma izleri eşit. Eşit boyutta xTruncate hatası silahlandırıldığı halde C ve Rust o çağrıyı yapmadı. Eksik uzunluk bir sayfanın altındaysa playback öncesinde ayrı uzatma yazımı yok. |

28 native akış ve 12 journal imajı her iki Rust kipinde eşit. Native matris C ile ilk kaydın xRead sınırını, DB xWrite/xTruncate izlerini, hata durumunu ve kalıcı DB baytlarını karşılaştırır. 12 dosya imajında aynı sayfanın iki geçerli kaydı, kesilmiş kayıt ve checksum uyuşmazlığı denetlendi.

V6b.43'ün dört gerçek C çöküş imajı ve önceki 147 WAL/sıcak journal kurtarma vakası tekrar eşit. Ham kanıt `/home/hakan/github/sqliters/target/perf-work/v6b47/res/` altında. [Açık farklar](gorulen_farklar_v6b_44.md).

| Doğrulama kapısı | Sonuç |
|---|---|
| Çalışma alanı, C ikilisi PATH'te | OFF 1144 geçti, 7 başarısız, 337 yok sayıldı. ON 1130 geçti, 21 başarısız, 337 yok sayıldı. Başarısız test kümeleri V6b.43 ile aynı. |
| DML, 6001–6400 | OFF 380/400, ON 400/400. Ham seed JSON'ları V6b.43 ile aynı. |
| Ayrışma bulucu | 245 stdout ve 244 stderr sonucu C ile eşit. V6b.43 ham özeti aynı. |
| Rust 1.100 tabanına hız | Yedi deneme. Toplam medyan taban 299,98 ms, aday 368,62 ms. Aday/taban 1,229. 14 digest eşit. |

Gcov kapsamındaki 56 C dalının alınmayan 20 çıkışı ayrı kaydedildi.
