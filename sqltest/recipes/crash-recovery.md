# Process kill ve recovery tarifi — NOT_RUN

Hiçbir process/DB/kill/recovery yürütülmez. Ayrı yetkili gelecekteki runner
bu tarifi uygularsa her worker PID'si ve ACK'si kaydedilmelidir. **Process kill
güç kesintisi değildir**; fsync/disk cache ve elektrik kesintisi garantisi
bu tariften çıkarılamaz.

Önkoşul: yalnız sqltest sandbox'ındaki fresh WAL DB; local FS/OS/lock modeli,
synchronous FULL, autockpt=0, compiler/binary/source/SQL/seed hash'leri sabit.
[WAL oturumları](../schedules/wal.json) için setup toplamı 300, writer commit
sonrası toplam 325 beklentisi vardır; bunlar doğrulanmamış beklentilerdir.

| Nokta | Gelecek kontrollü schedule | Beklenen invariant |
| --- | --- | --- |
| commit öncesi kill | Writer BEGIN IMMEDIATE + UPDATE; `BEFORE_COMMIT` ACK, dış parent yalnız o PID'yi kill/reap eder; COMMIT gönderilmez | Uncommitted +25 recovery sonrası görünmez; toplam 300 beklenir |
| commit sonrası kill | Writer COMMIT başarılı dönüşünden sonra `AFTER_COMMIT` ACK; ayrı işlem yapmadan PID kill/reap | Sözleşmedeki sync/local FS varsayımlarıyla committed toplam 325 beklenir |
| checkpoint sırasında bounded kill | Reader hold + writer commit; checkpointer gerçek backfill başlangıcına vardığını kanıtlayan event varsa kill | Tam committed prefix korunur; torn/partial state C ve Rust tarafından ayrı recovery kopyalarında doğrulanır |

Son satır yalnız SQL komutunun gönderilmesiyle kanıtlanamaz: engine/OS event
veya ayrı fault runner yoksa NOT_RUN kalır. “Kill oldu” bütün checkpoint/crash
noktalarının tüketildiğini göstermez. Torn write/power loss simülasyonu veya
C `testvfs` noop'u burada yoktur.

Kurtarma önce hasarlı DB/WAL/SHM/journal bundle'ın ham hash'li korunmasıyla
başlar. C ve Rust aynı bundle'ın ayrı kopyalarını açar; committed-prefix satır
çokluğu/tag/içerik, integrity_check, header/frame checksum/salt, reader snapshot
ve sidecar değişimleri karşılaştırılır. Aynı hasarlı kopyayı önce C ile açıp
sonra Rust'a vermek eşdeğer girdi değildir. Kurtarma süresi ve event sırası
ileriki gerçek koşuda kaydedilir; bu görevde ölçülmedi.
