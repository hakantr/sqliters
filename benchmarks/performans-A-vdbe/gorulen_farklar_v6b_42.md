# V6b.42 — Açık farklar ve kapsam

V6b.41'de kaydedilen D145 EXCLUSIVE DELETE unlink sırası ve E28 WAL header/payload yazma boyutu ve ofset farkları, ölçülen akışlarda kapandı. Native EXCLUSIVE DELETE vakalarında CLOSE sırasında xDelete veya journal xSync hatası sonrası kalan dosya baytları da karşılaştırıldı. TRUNCATE ve PERSIST'in özel platform yolları, EXCLUSIVE super-journal, tahsis hatası, WAL SHM hata enjeksiyonu ve daha geniş işletim sistemi ve dosya sistemi hata alanları açık.

D146 sıcak journal uygunluğu. Aynı ada ait sıfır baytlık bir DB ve daha önceki dosyadan kalmış geçerli bir rollback journal ile C journal'ı kaldırıp boş veritabanını açıyor. V6b.42 Rust eski tabloyu yeniden oluşturuyor. 512, 4096, 8192 ve 65536 baytlık dört imajda iki yürütücüde tekrarlandı. Aynı journal dolu DB'ye verildiğindeki dört pozitif kontrol C ile aynı. Kanıt `/home/hakan/github/sqliters/target/perf-work/v6b46/res/baseline.json`.

F13–F15 dahil V6b.41 test beklentileri korunuyor. D52, V1/M4, kalan DDL ve PRAGMA davranışları ile E28'in diğer pager kurtarma yolları genel C eşliği açısından açık. [Karar ve doğrulama](karar_esligi_v6b_42.md).
