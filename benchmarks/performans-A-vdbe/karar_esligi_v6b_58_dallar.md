# V6b.58 — D162 C dal kapsamı

40 ana iki dosya vakası, 35 tek dosya veya commit noktası vakası ve sekiz guard açılış vakası ayrı C gcov profillerinde ölçüldü. Odak aralıkları `pager.c:2457–2478,2608–2638,2642–2678,2888–2905,3031–3046,6465–6469`. Üç profil birlikte 76 çıkışın 55'ini aldı.

| C satırı ve dal | Alınmayan karar |
|---|---|
| 2460/1 | Subjournal kaydı yolu. Bu rollback profilleri ana journal kayıtlarını kullanıyor. |
| 2463/0 | Subjournal kaydında cache sayfası var. |
| 2463/1 | Subjournal kaydında cache sayfası yok. |
| 2463/2 | Subjournal kaydındaki sayfada NEED_SYNC yok. |
| 2463/3 | Subjournal kaydındaki sayfada NEED_SYNC var. |
| 2465/1 | Geri yazım değerlendirmesinde ana DB tanıtıcısı kapalı. |
| 2466/2 | Ana DB'ye yazmadan önce PAGER_OPEN geri yükleme yolu. |
| 2610/0 | Guard dosya nesnesi tahsisi başarısız. |
| 2626/0 | Super-journal boyutu okunamadı. |
| 2629/0 | Super-journal ad listesi için tahsis başarısız. |
| 2638/0 | Super-journal ad listesi okunamadı. |
| 2649/0 | Bir kardeş journal'ın varlık sorgusu hata verdi. |
| 2665/0 | Mevcut kardeş journal açılamadı. Super-journal açılış hatası ayrı profilde alındı. |
| 2671/0 | Açılmış kardeş journal'ın super-journal kaydı okunamadı. |
| 2676/3 | Kardeş journal'ın geçerli işaretçisi farklı bir super-journal'ı gösterdi. |
| 2890/0 | Playback başında journal boyutu okunamadı. |
| 2900/1 | Playback başında super-journal kaydı okuma hatası. |
| 2903/1 | Super-journal sorgusu hata verdi. Dosyanın başarıyla yok olarak görülmesi ayrı olarak ölçüldü. |
| 3031/1 | DB sync öncesinde playback hata sonucu taşıdı. |
| 3036/1 | Playback veya rollback sync hatası katılımcı sonlandırmasını atladı. |
| 3040/1 | Katılımcı sonlandırma hatası super-journal guard'ını atladı. |

Ölçülen commit noktası kararı `2903` ek profilde `34, 0, 8, 26`, guard açılış kararı `2618` hata profilinde `8, 4`, phase-one sync kararı `6467` ana profilde `88, 8, 88, 0`. Tüm aralıkların ham dal sayaçları ve kaynak satırları `target/perf-work/d162-proof/final-worktree/res/verify.txt` altında.
