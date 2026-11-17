# V6b.18 — D119

Sayfa1 IOERR_READ yutulması giderildi. Ortak okuyucu yürütme hatası işlemi geri alırken başarısız reprepare işlemi açık bırakır. 56 yürütme, 16 hazırlama ve 64 hook/sayaç akışının sonuçları/durumları ve .db baytları C ile aynı. 13.181 program aynı; API 4157/4158 aynı (eski D103); bütün 4158 akışın .db baytları aynı.

İlk ara transaction matrisinin tam eski ham dosyası yeniden koşmada üzerine yazılmıştır; eldeki araç çıktısı özeti açıkça etiketlenmiştir. Son bütün ham matrisler saklıdır. Başarısız reprepare ayrımının önceki ham çıktısı da saklıdır.

D118 kısa okuma, E28 COMMIT/pager hata durumu, diğer özel hata türleri ve eski pubstep hızlı yollarının genel denetimi açık. VFS izleri bütünüyle eşit sayılmadı. Kanıt [karar_esligi_v6b_18.md](karar_esligi_v6b_18.md) içinde.
