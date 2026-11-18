# V6b.68 — WAL restart sonrası SHM indeks hücreleri

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. Başlangıç V6b.67 `4926f76aee7104a38e0ec09b3d938f9bf1c191bc`.

| C kararı | C sonucu | Rust karşılığı |
|---|---|---|
| `wal.c:2234–2248`, `2445–2467` | TRUNCATE uygun okuyucu kilitlerini aldığında boş canlı snapshot, yeni salt ve readmark yayımlıyor. İndeks bloklarının eski hücreleri kalıyor. | Shared WAL restart yalnız başlık ve readmark yayınlıyor. WAL boyutu checkpoint yolunda C gibi sıfıra indiriliyor. |
| `wal.c:3961–3985` | Olağan yazıcı mevcut WAL başına dönebildiğinde aynı restart yordamını kullanıyor. Pozitif okuyucu kilidi BUSY ise mevcut WAL'a devam ediyor. | Ortak restart yolu aynı başlık kararını kullanıyor. İndeks slot temizliği bölgenin ilk append'ine taşınıyor. |
| `wal.c:1353–1358` | İndeks bölgesindeki ilk frame, o bölgenin sayfa ve hash tablolarını temizler. Diğer bölgeler kendi ilk frame'lerine kadar eski hücrelerle kalır. | Mevcut `record_frame_internal` ilk frame temizliğini yapıyor. Snapshot lookup ve checkpoint frame aralıkları yayımlanan canlı sınırı kullanıyor. |

TRUNCATE ve doğal writer restart, okuyucu yok/READ0/pozitif WAL okuyucu ve bir/iki indeks bölgesi için 12 disk vakası ölçüldü. Her iki Rust kipinde 12/12 eşit. Önce ve sonra 630'ar ham dosya özeti yeniden hesaplandı. 16 eski indeks hücresi ayrışması kapandı. SQL ve okuyucu snapshot'ları, checkpoint sonucu, ana DB SHA, geçerli WAL frame payload ve checksum'ları ile eski WAL tail özetleri eş. WAL ve SHM'deki rastgele salt ve bağlı checksum'lar ayrı doğrulandı.

Pozitif okuyucu, TRUNCATE için BUSY ve yazıcı için eski nesle append sonucu verdi. READ0 okuyucusu eski ana DB snapshot'ını tutarken restart başarılı. Sıfır `mxFrame` altında eski hücreler görünür frame olarak seçilmiyor. Sonraki ilk frame block 0'ı temizliyor. İkinci bölge kendi ilk frame'ine kadar önceki hücreleri tutuyor. Kalıcı iki bölgeli regresyon iki kipte geçti. [C dal kapsamı](karar_esligi_v6b_68_dallar.md) · [Açık farklar](gorulen_farklar_v6b_68.md).

| Kapı | Sonuç |
|---|---|
| Çalışma alanı | OFF 1167 geçen, 6 başarısız, 337 yok sayılan. ON 1154 geçen, 19 başarısız, 337 yok sayılan. Yeni iki bölgeli regresyon geçti. Başarısız kümeler V6b.67 ile aynı. |
| DML fuzzer | OFF 380/400, ON 400/400. Ham özet V6b.67 ile aynı. |
| Ayrışma bulucu | 245/245 stdout, 244/245 stderr. Ham özet V6b.67 ile aynı. |
| Hız ve sonuç digest'i | 306,07 ms taban, 377,07 ms aday, 1,232 kat. 14/14 digest eşit. |

147/147 kurtarma, 12/12 dosya imajı ve 4/4 gerçek C çöküşü kontrolü eşit. Ham kanıt `/home/hakan/github/sqliters/target/perf-work/wal-restart-slots-proof/res` ve `/home/hakan/github/sqliters/target/perf-work/wal-restart-verify/res` altında.
