# V6b.50 — D154 normal rollback-journal COMMIT sonu kilit sırası

Referans SQLite 3.54.1, source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. Rust 1.100.0. [C dal kapsamı](karar_esligi_v6b_50_dallar.md).

| C yeri ve koşulu | C sonucu | Rust karşılığı ve kanıt |
|---|---|---|
| `pager.c:2090–2112,2193–2217,6800–6801`, normal disk yazıcı | Journal sonlandırması ve gerekiyorsa ana dosya küçültmesi kilit tutulurken yapılır. Normal bağlantı SHARED'a düşer. Boş yazıcı da normal adımlardan geçer. | `commit_phase_two_inner` (`lib.rs:5350–5398`) ve `finalize_rollback_commit` (`lib.rs:5401`) mevcut finalizasyonu normal DELETE, PERSIST ve TRUNCATE için tek SHARED girişimiyle birleştirir. Boş yazıcı, FULL/NORMAL ve journal modları ölçüldü. |
| `pager.c:2209–2217`, finalizasyon ve SHARED hata önceliği | Journal finalizasyonu hata verirse SHARED yine denenir. İki hata varsa finalizasyon sonucu döner. | `lib.rs:5382–5396` finalizasyon sonucunu SHARED sonrasına saklar ve ilk hatayı döndürür. Tek ve birleşik hata, okur açıkken ve serbestken ölçüldü. |
| `btree.c:4370–4400,4432–4456` ve `pager.c:5533–5554,5861–5868`, son okuyucu | Okuyucusu kalan bağlantı SHARED'da kalır. Son sayfa referansı bırakılınca NO_LOCK yapılır. Okuyucu kalmazsa aynı sonlandırmada SHARED'dan sonra NO_LOCK gelir. | `lib.rs:5385–5397,4884–4920` tutulacak okur için tek SHARED sonrası Read durumunu kurar. Son okur mevcut kapanış yoluyla NO_LOCK'a iner. 24 sağlıklı SQL vakasında 12 anında ve 12 gecikmeli NO_LOCK ile veri ve DB özetleri eşit. |

Ölçülen terminal kilit ve journal sonlandırma izinde iki kip 24/24 sağlıklı vakayı eşledi. Ayrı aday koşusu 36 phase-two ve 18 açık-okur vakasında sonlandırma sırası, SQL sonucu ve ana dosya özetlerini C ile eşledi. Ham journal nonce ve phase-one yazma izleri ayrı kaydedildi. Tüm ham `FCNTL 22` olayları C'de görünüyor ve D155 kapsamında açık. Ham kayıtlar `/home/hakan/github/sqliters/target/perf-work/v6b54-proof/` ve `/home/hakan/github/sqliters/target/perf-work/v6b54_pager/` altında. [Açık farklar](gorulen_farklar_v6b_50.md).

| Doğrulama kapısı | Sonuç |
|---|---|
| Çalışma alanı, C ikilisi PATH'te | OFF 1147 geçti, 7 başarısız, 337 yok sayıldı. ON 1133 geçti, 21 başarısız, 337 yok sayıldı. Genişletilen commit testi iki kipte geçti. Başarısız kümeler V6b.49 ile aynı. |
| DML, 6001–6400 | OFF 380/400, ON 400/400. Ham seed JSON'ları V6b.49 ile aynı. |
| Ayrışma bulucu | C ile 245 stdout ve 244 stderr sonucu eşit. V6b.49 ham özeti aynı. |
| Rust 1.100 tabanına hız | Yedi deneme. Toplam medyan taban 300,95 ms, aday 375,24 ms. Aday/taban 1,247. 14 digest eşit. |

D153'ün 18 kapanış vakası, önceki 147 kurtarma, 12 journal imajı ve dört gerçek C çöküşü tekrar eşit. Dört kapı ve üretim SHA-256 kayıtları `/home/hakan/github/sqliters/target/perf-work/v6b54_verify/` altında.
