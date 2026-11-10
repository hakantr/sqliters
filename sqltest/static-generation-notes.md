# Adım 1 statik teslim kaydı

Bu kayıt SQL/test sonucu değildir. Yalnız generator ile dosya yazıldı ve
dosya içerikleri/hash/sayıları incelendi. **SQL/DB/test/build/benchmark NOT_RUN.**

- 75 okunabilir bağımsız senaryo, yedisi `negative-*` kasıtlı hata beklentisi.
- MAX_COLUMN 128/2000/4096 için sekiz aile × üç sınır = 72 fiziksel SQL dosyası.
- 10.000 normal kaydın fiziksel CSV, JSONL ve literal INSERT SQL karşılıkları.
- İlişkili fixture: customers 3000, orders 12000, items 24000, employees 1000,
  measurements 10000; toplam 50.000 fiziksel kayıt, CSV/JSONL/literal SQL.
- Orders'ta 705 NULL ve 389 unmatched customer anahtarı; items'ta 774 NULL
  ve 628 unmatched order anahtarı. Employees sekiz root, id'si kendinden küçük
  parent kuralı taşır. Bu sayılar üretilmiş veriyi anlatır; JOIN motor sonucu değil.
- Aynı büyük şemaya bağlı 13 açık SELECT: tüm alanları okuma, FULL/LEFT JOIN,
  aggregation, semi/anti, recursive manager ve zaman penceresi.
- Wide 2000/4096 için explicit DDL, 2000/4096 INSERT SELECT ifadesi ve bütün
  alan SELECT'i. 3000'er kayıt üretme hedefi NOT_RUN; DB dosyası oluşturulmadı.

Statik incelemede CSV/JSONL logical kayıt sayısı, CSV alan sayısı, wide DDL/
ifade/projection sayıları ve manifest byte/SHA bağları dosyalardan okundu.
Üretilmiş SQL'de placeholder üç nokta yoktur. Generator yalnız csv/hashlib/io/
json/pathlib import eder; motor/subprocess/runner yoktur. Aynı kaynak ve
profiles ile ardışık üretimde corpus manifesti byte-identical olarak incelendi.
SQL grameri, hata beklentileri veya engine capability bu incelemeyle doğrulanmadı.

Büyük SQL/CSV/JSONL ignored `artifacts/generated/full/` altında fiziksel bulunur;
generator ile yeniden üretilebilir. Kaynak, profiles, küçük örnekler, corpus
manifesti ve scope matrisi review içindir. Bütün yazmalar yalnız yeni sqltest
kapsamında; kullanıcı kaynaklarını veya R-0006 işlerini değiştirmedik.
Ebeveyn statik içerik/hash/sayım incelemesi tamamlandı; adım 2 bu kayıt sırasında henüz başlamadı.

## Adım 2 son statik teslim

128/2000/4096 geniş tablo ve workload SQL setleri; 22 ek scope kaydı, optional
JSON/JSONB/FTS5/R-Tree, ayrı WAL oturumları ve contention, API ve crash/recovery
tarifleri hazırlandı. Toplam 216 manifest dosyası / 15.485.945 byte.
Ebeveyn dosya kimliklerini, kaynak generator hash bağını, scope yollarını ve
yerel Markdown bağlantılarını statik olarak inceledi. Motor/SQL/test/derleme
yürütülmedi; bütün sonuçlar NOT_RUN. Büyük generated dosyalar ignored halde
diskte bulunur; kaynak ve komut setleri yalnız sqltest kapsamıyla commit edilir.
