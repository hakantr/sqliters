# V6b.51 — D155 normal COMMIT phase-two VFS bildirimi

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. [C dal kapsamı](karar_esligi_v6b_51_dallar.md).

| C yeri ve koşulu | C sonucu | Rust karşılığı ve kanıt |
|---|---|---|
| `pager.c:2193–2207,6799–6801`, başarılı yazıcı COMMIT | Başarılı journal sonlandırması ve gerekli DB küçültmesinden sonra `SQLITE_FCNTL_COMMIT_PHASETWO` NULL ile çağrılır. NOTFOUND başarıya dönüşür. | `VfsFile::notify_commit_phase_two` (`sqliters-vfs/src/lib.rs:206–210`) bildirimi tanımlar. Normal disk DELETE, PERSIST ve TRUNCATE için `sqliters-pager/src/lib.rs:5382–5387,5396–5403` aynı sırayı uygular. Sağlıklı 24 vaka ve açık OK veya NOTFOUND sonuçları ölçüldü. |
| `pager.c:2204–2217`, önceki sonuç ve SHARED | Önceki sonlandırma veya kısaltma hatası bildirimi atlar. Bildirim hata verse de normal kilitte SHARED denenir. Önceki hata SHARED sonucundan önce gelir. | `lib.rs:5382–5404` ayrı sonuçları tutar. Özel test VFS'indeki sonlandırma, bildirim ve SHARED hata kombinasyonlarında sonuç ve sıra eşit. |
| `pager.c:2214–2217,6799–6801`, bildirimden önce commit noktası geçilmiş | Journal sonlandırılmıştır. Bildirim hata dönerse ana dosyadaki commit geri alınmaz. Pager okuyucu durumuna geçer ve G/Ç tipi sonuç gereken ERROR yoluna taşınır. | `lib.rs:5396–5403` bildirim hatasında üst cleanup'a Read durumu bırakır. Sonraki sorgu ve kalıcı DB SHA-256, üç normal rollback kipinin ölçülen callback hata vakalarında C ile eşit. |

Gerçek kaynaktan 24 sağlıklı vaka C ile her iki Rust kipinde aynı phase-two bildirimi, SHARED/NO_LOCK, okuyucu bırakma ve DB özetini verdi. Ayrı scratch adayında 60 tek dosya callback ve hata önceliği vakası eşit. D154'ün 36 phase-two ve 18 açık-okur aday vakası korundu. Ham günlük xWrite ve bağımsız PERSIST nonce izleri kayıtlarda duruyor. [Açık farklar](gorulen_farklar_v6b_51.md).

## Dört doğrulama kapısı

- Rust 1.100 çalışma alanı OFF 1148 geçti, 7 başarısız, 337 yok sayıldı. ON 1134 geçti, 21 başarısız, 337 yok sayıldı. Başarısız test adları V6b.50 ile aynı. Yeni `commit_phase_two_notifies_after_delete_and_preserves_committed_data` testi iki kipte de geçti.
- DML seed 6001–6400, OFF 380/400 ve ON 400/400. Her iki ham kayıt V6b.50 ile aynı.
- Ayrışma bulucuda 245/245 stdout ve 244/245 stderr eşit. Ham sonuç V6b.50 ile aynı.
- Rust 1.100 zaman kapısında yedi dönüşümlü ölçüm ve 14 iş yükü digest'i eşit. Taban 297,74 ms, aday 369,92 ms, süre oranı 1,242.

Önceki 147/147 kesinti ve kurtarma vakası, 12/12 dosya imajı ve 4/4 gerçek C çöküş görüntüsü eşit. Kaynak özeti ve dört kapı kayıtları `target/perf-work/v6b55_verify/res` altında, gerçek worktree native izleri `target/perf-work/v6b55-proof/final-worktree/res` altında.
