# Ayrı WAL oturumları — NOT_RUN

[wal.json](wal.json) iki mod tarifler: tek süreçte iki bağımsız bağlantı ve
gerçek üç ayrı process. Reader/writer/checkpointer dosyaları birbirine
concatenate edilmez. Aynı DB yolunu alan gerçek orchestrator bu teslimde yoktur.

Writer setup bağlantısı açık tutulur; böylece son bağlantı kapanışının
checkpoint/sidecar etkisi initial durumu değiştirmez. Reader BEGIN sonrasında
SELECT yaparak snapshot'ı gerçekten açar. Writer commit etmeden reader ACK'si
gerekir. Commit sonrası reader eski toplamı, reader transaction bittikten
sonraki SELECT yeni toplamı görmelidir. Beklenen sayılar doğrulanmadı.

İki bağlantı modunda checkpointer rolü writer bağlantısının alias'ıdır;
üç süreç modunda kendi PID/bağlantısı vardır. Setup DB'si yalnız sqltest
sandbox'ta ve WAL'a uygun yerel FS üzerinde gelecekte hazırlanır. Bütün
bağlantılarda busy timeout 50 ms; dış step timeout profile'da 30 saniyedir.
BUSY cevabı süreç timeout'u yerine geçmez; koşu timeout olursa açık TIMEOUT.

Her numaralı adımdan sonra orchestrator, gerçek prepare/step/Done/Row veya
transaction durumunu okuyup belirtilen ACK'yi kaydeder; SQL yorumundaki ACK
etiketi gerçek event kanıtı değildir. Tekrarlanan/eksik/out-of-order ACK, aynı
PID'lerin üç süreç gibi sayılması ve role'süz erken worker exit reddedilmelidir.
Checkpoint counts için tam sayfa sayısı varsayılmaz; log/backfill ve busy
invariant'ları engine sonucu ve reader ömrüyle doğrulanır.

Bu tarif uygulama, test sonucu veya concurrency doğrulaması değildir.


Her role/connection, writer setup WAL'a geçtikten ve kendi transaction'ı
başlamadan önce `sessions/wal/connection-config.sql` preflight'ını gelecekte
uygular. journal_mode=WAL, synchronous=FULL, autockpt=0 ve busy_timeout=50
**her bağlantıda geri okunur**; writer ayarının reader/checkpointer'a geçtiği
varsayılmaz. Bu önkoşul ana dokuz ACK adımını değiştirmez. Ek BUSY_SNAPSHOT
ve iki-writer BUSY tarifleri [ayrı contention recipe](../recipes/wal-contention.md)
içindedir; ana schedule'la concatenate edilmez. Bütün sonuçlar NOT_RUN.
