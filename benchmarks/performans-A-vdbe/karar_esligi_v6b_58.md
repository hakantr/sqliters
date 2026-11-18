# V6b.58 — D162 iki dosyalı COMMIT ve aktif rollback

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. [C dal kapsamı](karar_esligi_v6b_58_dallar.md).

| C yeri ve koşulu | C sonucu | Rust karşılığı |
|---|---|---|
| `pager.c:6462–6469,6735`, phase-one sync | C, ana DB VFS'ine super-journal yoluyla SYNC bildirimi gönderir. NOTFOUND başarıya çevrilir. Başka bir hata fiziksel sync öncesinde döner. | `sqliters-pager/src/lib.rs:5331–5344` bunu disk rollback katılımcısı için uygular. Başarı, NOTFOUND ve G/Ç kararı ölçülür. |
| `pager.c:2899–2905`, rollback başlangıcı | Katılımcı journal'ındaki geçerli super-journal kaydı denetlenir. Super-journal silindiyse çok dosyalı COMMIT noktası geçilmiş ve eski veriyi yükleme gereği kalkmıştır. | `lib.rs:5649–5676,5690–5748` fiziksel journal kaydını ve dosyanın varlığını denetleyerek disk ve cache geri yüklemesini seçer. |
| `pager.c:3031–3045,2214–2217`, playback sonrası | Gereken ana DB sync ve journal sonlandırması yapılır. Normal kilit SHARED'a indirilir. Sonlandırma hatası önceliklidir. | `lib.rs:2067–2077,5714–5721,5771–5783` aktif ve sıcak playback için sync bildirimini ortak kullanır ve sonlandırma sonucunu korur. |
| `pager.c:2583–2689,3040–3046`, super-journal temizliği | Başarılı katılımcı sonlandırmasından sonra bağlı kardeş kalmadığı doğrulanır ve yetim super-journal kaldırılır. Açılış hatasında geçerli dosya korunur. | `lib.rs:5785–5799` mevcut guard'a bağlanır. Hata halinde pager okuyucu durumu ve gerekli bağlantı sonu kilit temizliği korunur. |

Kalıcı entegrasyon testi ikinci katılımcının phase-one SYNC reddinden sonra iki DB'nin eski baytlarını ve satırlarını, katılımcı journal'larının sırasını ve super-journal'ın son temizliğini denetliyor. [Açık farklar](gorulen_farklar_v6b_58.md).

Gerçek worktree'de 40/40 phase-one ve normal iki dosya akışı, 8/8 commit noktası silme hatası ve 8/8 guard açılış hatası iki Rust kipinde C ile eşit. COMMIT sonucu, yeniden açılış, bütünlük ve iki ana dosyanın özetleri karşılaştırıldı. Commit noktası öncesi hata eski veriyi geri getirdi. Silme sonrasındaki hata commit edilmiş veriyi korudu.

Tek dosyalı rollback 27/27 ana dosya içeriği ve yeniden açılışta eşit. Tam gözlem izi ve dosya kümesi 17/27 eşit. Dört ham PERSIST farkı bağımsız nonce checksum alanında ve altı spill yazım farkı önceki Rust sürümünde de var. C↔Rust kurtarmasını içeren 2PC entegrasyon paketi iki kipte 6/6 geçti. Ham kayıtlar `target/perf-work/d162-proof/final-worktree/res` altında.

## Dört doğrulama kapısı

- Rust 1.100 çalışma alanı OFF 1155 geçti, 7 başarısız, 337 yok sayıldı. ON 1141 geçti, 21 başarısız, 337 yok sayıldı. Başarısız test adları V6b.57 ile aynı ve D162 entegrasyonu iki kipte de geçti.
- DML seed 6001–6400, OFF 380/400 ve ON 400/400. Ham sonuçlar V6b.57 ile eşit.
- Ayrışma bulucuda 245/245 stdout ve 244/245 stderr eşit. Ham sonuç V6b.57 ile aynı.
- Rust 1.100 zaman kapısında yedi dönüşümlü ölçüm ve 14 eşit iş yükü digest'i. Taban 296,64 ms, aday 369,25 ms, süre oranı 1,245.

Önceki 147/147 kesinti ve kurtarma vakası, 12/12 dosya imajı ve 4/4 gerçek C çöküş görüntüsü eşit. Kaynak özeti ve dört kapı kayıtları `target/perf-work/d162-verify/res` altında.
