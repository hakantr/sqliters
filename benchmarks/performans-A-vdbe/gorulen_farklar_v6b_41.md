# V6b.41 — Açık farklar ve eski test beklentileri

| Kayıt | Kanıt ve sınır |
|---|---|
| D145 — EXCLUSIVE + DELETE | C `pager.c:2139–2142` EXCLUSIVE kilit tutulurken journal başlığını sıfırlar ve dosyayı kapanışa kadar tutar. Rust xDelete'i COMMIT'te çağırıyor. Aynı unlink hatası C'de kapanışta görülür ve satır 2 kalır. Rust'ta COMMIT 2570 verir ve sonraki okuyucu satır 1'i kurtarır. Matrisin bir açık SQL/API ve dosya farkı budur. |
| E28 — WAL VFS yazma boyutu | C `wal.c:4058–4062` 12392 ofsetine 24 bayt header ve 12416 ofsetine 4096 bayt payload yazar. Rust 12392'de 4120 baytlık bir çağrı yapar. Rust payload probu ilk 24 baytı yazıp aynı çağrıda hata döndürdü. Satırlar, kodlar, hook ve kalıcı dosyalar eşit. Ham VFS çağrıları farklı. |
| F13 — Y1 eski COMMIT beklentisi | `test_bulgu_y1_failed_commit_partial_writes_rolled_back_atomically` başarısız COMMIT'ten sonra işlemin açık kalmasını bekliyor. İki tabloya yazılan ve üçüncü fiziksel DB yazımı kesilen native probda C ve iki Rust kipi autocommit 1, tek hook, eski iki satır ve aynı dosya hash'i verdi. |
| F14 — Audit3 eski bağlantı zehirleme beklentisi | `test_audit3_autocommit_commit_ioerr_clean_rollback_no_false_poison_and_vacuum_schema_reload` ikinci G/Ç hatasından sonra bağlantının kalıcı olarak hata vermesini bekliyor. COMMIT ve geri yazım hatasının ardından enjeksiyon kalkınca C ve Rust aynı bağlantıda sabit sorguyu, hot journal kurtarmasını ve sonraki tablo okumasını tamamladı. |
| F15 — Pager finalization eski retry beklentisi | `test_journal_finalization_fault_injection_and_readonly_mode_transitions` ERROR sonrası PagerRollback'in hemen başarılı olmasını ve aynı pager işleminde COMMIT tekrarını bekliyor. Doğrudan C probu rollback ve retry'ın saklı 2570/1546/778/1034 ile döndüğünü gösterdi. PERSIST sync hatasından önce başlık sıfırlandığında C yeni satırı korudu. |

Y2'nin iki yönlü hot journal kurtarma testi, COMMIT sırasında başlayan otomatik rollback'e de hata uygulayacak şekilde aynı VFS düzeniyle çalıştırıldı. C'de ve Rust'ta journal'ın gerçekten sıcak kaldığı koşul yeniden kuruldu. Testin dosya, veri ve integrity beklentileri korundu.

Genel E28 kapsamı açık. Özel TEMP, MEMORY ve OFF hata durumları, kapanışta journal sync, sıcak journal RW açılış yarışları, WAL SHM hata enjeksiyonu, super-journal temizliği ve ERROR anında etkin mmap lease için ayrı C karşılaştırması gerekiyor. Geçmiş D52, kalan V1/DDL ve PRAGMA farkları ile M4 çalışması sürüyor. [Karar ve doğrulama](karar_esligi_v6b_41.md).
