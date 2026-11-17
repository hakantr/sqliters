# V6b.33 — VFS fetch/unfetch ve aktif eşleme sınırı

VFS tarafında Unix `xFetch/xUnfetch` yaşamı eklendi. Gerçek read-only MAP_SHARED eşlemesi, bireysel RAII ref sayısı, eski limitin geri verilmesi, aktif ref varken limit güncellemesinin reddi, EOF256 ve toplu invalidation taşındı. Pager henüz fetch çağırmadığı için D142 kapanmadı. Sıradaki iş V6b.34 sayfalayıcı bağlantısıdır; kullanıcı sonraki işi başka sohbete devretmek istedi.

## C kararları ve Rust karşılıkları

Özgün C kaynak koşullarının tamamı, her gcov çıkışı ve Rust yeri [dal tablosunda](karar_esligi_v6b_33_dallar.md). Altı yordam/blokta 46 dalın31'i alındı; alınmayan15 dal tek tek sınıflandırıldı. `sizeof(size_t)<8` hedef dışı derleme koludur. Önceki büyük profil ile kapsam sayıları toplanmaz. Harita uzatma/mremap, fstat hata enjeksiyonu ve mmap başarısızlık günlüğü açıkça eksik/kanıtlanmamıştır; test geçmesi bunları kapatmaz.

## Sonuçlar ve dört kapı

| Denetim | Sonuç |
|---|---|
| C/model/üretim Rust VFS | 10.051 işlem aynı; seed354133; girdinin SHA256'ı `c9edb7a8b2d65c79bbd7fd02a519b75f1e78965e7d021e2433a9ffd712e2a99f` |
| Fixture bütün dosya | 417.792 bayt; üç driver sonrası SHA256 `9e8b1d28874493108d894cb1735f2cf73c5991bab51c6d3fdeffa58ff2af240b` |
| Yeni VFS yaşam testleri | 3/3; son ref, EOF/invalidation, remap/yeni yazılan veri |
| Native mmap | 240/250 aynı; altı D142 ve dört D144 açık; bütün kalıcı dosyalar aynı |
| TEMP | 270/270 aynı; anonymous TEMP ham bayt dökümü önceki E27 açıklığıdır |
| Tam mmap programı | 144/144; program header/opcode/P4/subprogram ve API sonuçları aynı |
| Elle yazılan CLI | İki anahtarda16 örnek aynı; stdout/stderr/exit ve bütün db hashleri |
| Çalışma alanı OFF | 1131 geçti / 4 başarısız / 337 yok sayıldı |
| Çalışma alanı ON | 1117 geçti / 18 başarısız / 337 yok sayıldı |
| DML400 | OFF378, ON398 aynı; önceki adımın bütün sınıf/seed JSON'u aynen korundu |
| Finder245 | stdout245, stderr244 aynı; önceki raporla tam aynı iki eski ayrışma |
| Hız Rust1.100 tabanına | 7 interleaved ölçüm; toplam 1.210x;14 digest farkı0 |

Eski başarısız test kümeleri V6b.32 ile aynıdır; üç yeni test her iki anahtarda geçti. F4/F6–F12 beklentileri değiştirilmedi; başarısızlıklar yeşil sayılmadı. DML/finder kusurları açık tutuldu. Sadece bu kapsamdaki değişiklikler doğrulandı; tüm C eşliği iddiası yoktur.

## Tasarım ve sınırlar

`crates/sqliters-vfs/src/fetch.rs`, limit/harita/outstanding durumunu tek mutex altında tutar. OsFile fd'si kapatılmadan harita bırakılır. `sys/unix.rs` içindeki mevcut izinli FFI sınırına ReadMapping eklenir; mmap/munmap sahipliği ve Send gerekçesi kaynakta açıklanır. Pager'a ham pointer veya mapped Rust slice verilmez: owned Vec ve ayrı lease verilir. Bu kopyalama bir performans iyileştirmesi olarak sunulmaz. `FetchPage::data()` snapshot'tır; genel C raw pointer ABI'si değildir. Cursor ve aynı bağlantıda read/write yaşamı sonraki işte kanıtlanmalıdır.

Mmap başarısızlığında limit0/fallback karşılığı vardır; C'nin unixLogError(SQLITE_OK) uyarısı henüz yoktur. Fstat hata kodu Result ile aktarılır ama ileti/syscall izi enjekte edilip eşitlenmedi. Mevcut eşlemeyi uzatma/mremap ve Windows fetch uygulanmış sayılmaz. NOMEM, özel VFS iVersion ve E28 pager ERROR ayrıca açıktır.

## Tekrar üretim ve kayıt

Yerel `/home/hakan/github/sqliters/target/perf-work/v6b33/`: `fetch_matrix_product.py`, `fetch_probe.c`, `fetch-r/`, `fetch_model.rs`, `fetch_proof.py`, `check.sh`, `gates.sh`, `manual_cli.py`, `mmap_known.py`, `res/validation.txt`, `source_manifest.json`, `oracle_and_binaries.json`. Kaynak manifestindeki dört dosya doğrulamalardan sonra değişmedi. C kimliği `3.54.1|2026-10-16 14:45:20 555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`; araç zinciri `rustc 1.100.0 (0ef1bc386 2026-11-09)`. Özgün `sqlite/` ve ana checkout değiştirilmedi.

Erken native denemede driver derlenmeden çalıştırılmıştı; FileNotFoundError sonuçları `res/early_missing_driver_*` altında saklandı. Driver tamamlandıktan sonraki tam native tekrar geçerli sonuçtur; erken hata test geçişi diye sayılmadı.

[Açık farklar](gorulen_farklar_v6b_33.md) · [Yeni sohbet için tam devir](devir_v6b_33.md)
