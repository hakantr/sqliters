# V6b.69 — CHECK BLOB kaynağı C dal kapsamı

| C yeri | Ölçülen karar |
|---|---|
| `vdbe.c:3237` | Kısa kayıt profili 102 eksik fiziksel slot, 133 mevcut slot. |
| `vdbe.c:3240` | 66 `P4_MEM` varsayılan kopyası, 36 NULL. |
| `vdbe.c:3337–3338` | 138 fiziksel encoding ataması ve 124 büyütülen, 14 hazır tampon. |
| `insert.c:2060–2092` | Fiziksel DML profilinde CHECK denetimine 36 giriş, OR IGNORE 9, diğer çatışma sonucu 27. |
| `pragma.c:2040–2055` | Kısa kayıt okuma profilinde 44 CHECK derleme geçişi. |

| Alınmayan ilgili C çıkışı | Sınıflandırma |
|---|---|
| `vdbe.c:3339`, `3341` | Fiziksel BLOB uzunluk limiti ve tampon tahsisi hatası. |
| `insert.c:2060`, `pragma.c:2040` | Etkin `ignore_check_constraints` bu CHECK profilinde kapalı. |
| `insert.c:2070–2074` | Güncellenen sütuna başvurmayan CHECK atlama yolu. |
| `insert.c:2077–2082` | Affinity önceden uygulandı veya CHECK derleme tahsisi başarısız. |
| `insert.c:2090` | REPLACE için CHECK hata kipinin ABORT'a çevrilmesi. |
| `pragma.c:2042–2049` | Derleme tahsisi hatası ve birden fazla CHECK için önceki koşul döngüsü. |

Ölçülen varsayılan ve fiziksel değer yolları ayrı C profillerinde çalıştırıldı. [Varsayılan kapsamı](/home/hakan/github/sqliters/target/perf-work/check-blob-origin-proof/res/native/gcov-reads/scope.txt) · [Fiziksel DML kapsamı](/home/hakan/github/sqliters/target/perf-work/check-blob-origin-proof/res/native/physical/gcov-dml/scope.txt) · [Fiziksel scan kapsamı](/home/hakan/github/sqliters/target/perf-work/check-blob-origin-proof/res/native/physical/gcov-stored/scope.txt).
