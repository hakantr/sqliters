<h1 align="center">sqliters Kaynak Deposu</h1>

<p align="center"><a href="README.md">English</a> | <strong>Türkçe</strong></p>

Bu depo, [SQLite veritabanı motoru](https://sqlite.org/) (referans sürüm:
SQLite 3.54.1) temel alınarak sıfır harici bağımlılıkla (*zero external
dependencies*) Rust dilinde geliştirilen bağımsız **sqliters** portunun tüm
çalışma alanı (*workspace*) kaynak kodunu ve uyarlanmış test paketini içerir.

SQLite'ın SQL lehçesi ve dosya biçimi hakkında genel kullanıcı belgeleri için
resmi [SQLite dokümantasyonuna](https://sqlite.org/) bakabilirsiniz. Bu README
dosyası; `sqliters` kaynak ağacını, deneysel niteliğini, lisanslama yapısını ve
Rust crate'lerinin SQLite mimarisiyle nasıl eşleştiğini açıklar.

> [!WARNING]
> **Deneysel Proje — Ciddi / Üretim Ortamları İçin Garanti Verilmez**
>
> `sqliters`, **deneysel** bir mühendislik ve araştırma projesidir. Deneysel
> projelerde, prototipleme çalışmalarında, testlerde ve veritabanı iç yapısını
> Rust üzerinde incelemek amacıyla kullanıma uygundur. **Ciddi, kritik veya
> üretim (*production*) projelerinde kullanım için veri güvenliği, çökme
> dayanıklılığı, eksiksizlik veya amaca uygunluk konusunda hiçbir garanti
> verilmez.** Kritik ve ciddi projeler için resmi
> [SQLite](https://sqlite.org/) kütüphanesini kullanınız.

## Bağımsız Proje Bildirimi

`sqliters`; SQLite'ın kamu malı (*public domain*) kaynak kodu, dosya biçimi
belirtimi ve test paketinden yararlanılarak sıfırdan Rust ile yazılmış bağımsız
bir uyarlamadır. Resmi bir SQLite ürünü **değildir**; SQLite geliştirici ekibi
veya Hipp, Wyrick & Company, Inc. ile herhangi bir bağlantısı ya da resmi onayı
bulunmamaktadır.

## Kamu Malı (Public Domain) ve Lisanslama

Bu depodaki `sqliters` kaynak kodu **Kamu Malı (*Public Domain*)** olarak
sunulmuştur (bkz. [LICENSE.tr.md](LICENSE.tr.md) / [LICENSE.md](LICENSE.md)):

> Yazar, bu kaynak kod üzerindeki telif haklarından feragat eder. Yasal bir
> bildirim yerine şu dilek paylaşılır:
>
>   *   Kötülük değil, iyilik yapmanız dileğiyle.
>   *   Kendiniz için bağışlanma bulmanız ve başkalarını bağışlamanız dileğiyle.
>   *   Verdiğinizden fazlasını almadan, özgürce paylaşmanız dileğiyle.

Doğrudan kamu malı feragatini tanımayan hukuk sistemleri için bu depo ek olarak,
açık kaynak dünyasındaki en serbest sıfır-koşullu lisanslar olan
**`Unlicense OR CC0-1.0 OR 0BSD`** seçenekleriyle, **"OLDUĞU GİBİ" (*AS IS*) ve
hiçbir garanti olmaksızın** lisanslanmıştır.

## Derleme ve Çalıştırma

`sqliters`, **sıfır harici crate bağımlılığıyla** (`yalnız std`; tüm üst
katmanlarda `#![forbid(unsafe_code)]`, yalnızca `crates/sqliters-vfs/src/sys/`
altında izole edilmiş işletim sistemi VFS/SHM sistem çağrılarıyla)
geliştirilmiştir.

Çalışma alanını ve komut satırı kabuğunu Cargo ile derlemek için:

        cargo build --workspace
        cargo build --release -p sqliters-cli

Etkileşimli `sqliters` komut satırı kabuğunu veya tek satırlık sorguları
çalıştırmak için:

        cargo run -p sqliters-cli -- ornek.db
        cargo run -p sqliters-cli -- -json ornek.db "SELECT sqlite_version();"

Birim testlerini, dokümantasyon testlerini, C SQLite süreçler arası uyumluluk
testlerini ve uyarlanmış Tcl test paketini çalıştırmak için:

        cargo test --workspace

### Kütüphane Kullanım Örneği

```rust
use sqliters::{Connection, Value};

fn main() -> sqliters::Result<()> {
    let mut conn = Connection::open_in_memory()?;

    conn.execute(
        "CREATE TABLE urunler (
             id INTEGER PRIMARY KEY,
             ad TEXT NOT NULL,
             adet INT NOT NULL
         );
         INSERT INTO urunler (ad, adet) VALUES ('alfa', 10), ('beta', 25);",
    )?;

    let sonuc = conn.query(
        "SELECT id, ad FROM urunler WHERE adet >= ?1 ORDER BY id;",
        &[Value::Integer(15)],
    )?;

    for satir in &sonuc.rows {
        println!("#{}: {}", satir.get_i64(0).unwrap(), satir.get_str(1).unwrap());
    }
    Ok(())
}
```

## Kaynak Ağacı Haritası

  *  **src/** - Genel Rust API'sini (`Connection`, `Statement`, `Row`, `Value`,
     `ErrorCode`, `Vfs`) dışa aktaran kök kütüphane crate'i (`sqliters`).

  *  **crates/sqliters-types/** - Temel veri tipleri, birincil/genişletilmiş
     hata kodları, sınırlar, 1..=9 bayt big-endian `varint`, `Value` tür
     yakınlıkları (*affinity*) ve karşılaştırma kuralları, metin kodlamaları
     (`UTF-8`, `UTF-16le`, `UTF-16be`) ile Format 4 kayıt kodlama/çözme yapısı.

  *  **crates/sqliters-vfs/** - Takılabilir Sanal Dosya Sistemi (`Vfs`,
     `VfsFile`), POSIX `fcntl` ve Win32 bayt aralığı kilitlemesi, WAL paylaşımlı
     bellek (`WalShm`) eşgüdümü ve `MemoryVfs`. İşletim sistemi FFI çağrıları
     yalnızca `crates/sqliters-vfs/src/sys/` modülüyle sınırlandırılmıştır.

  *  **crates/sqliters-pager/** - Sayfa önbelleği, 100 baytlık SQLite Format 3
     veritabanı başlığı, *rollback journal* kipleri, sıcak günlük (*hot-journal*)
     çökme kurtarması ve *savepoint* yönetimi.

  *  **crates/sqliters-btree/** - SQLite Format 3 Tablo B+Tree ve İndeks B-Tree
     depolama motoru, sayfa bölünmesi/yeniden dengelenmesi, taşma (*overflow*)
     zincirleri, boş sayfa listesi (*freelist*) ve `PRAGMA integrity_check`.

  *  **crates/sqliters-syntax/** - SQLite 3.54.1 SQL sözcük çözümleyicisi,
     `is_complete` durum makinesi, AST tanımları ve özyinelemeli-iniş / Pratt
     ayrıştırıcısı.

  *  **crates/sqliters-vdbe/** - Yazmaç tabanlı Sanal Veritabanı Motoru
     (`VdbeVm`), `EXPLAIN` baytkod dökümü, sıralayıcılar ve yerleşik skaler,
     kümeleme ve UTC tarih/saat fonksiyonları.

  *  **crates/sqliters-core/** - Şema kataloğu (`sqlite_schema`), SQL→VDBE
     derleyicisi, ilişkisel yürütme motoru, yabancı anahtar (*foreign key*) ve
     çakışma kuralları ile `Connection` / `Statement` yaşam döngüsü.

  *  **crates/sqliters-cli/** - Etkileşimli REPL ve nokta komutlarını
     (`.tables`, `.schema`, `.dbinfo`, `.dump`, `.mode`) destekleyen `sqliters`
     komut satırı kabuğu.

  *  **crates/sqliters-tcltest/** ve **tests/** - Sıfırdan Rust ile yazılmış
     Tcl altkümesi test koşucusu, kamu malı SQLite `.test` uyarlamaları ve
     süreçler arası C uyumluluk/regresyon testleri.

## Modüller Nasıl Bir Araya Gelir?

`sqliters` modüler bir tasarıma sahiptir. Mimari arka plan için resmi
[SQLite mimarisi](https://sqlite.org/arch.html),
[SQLite veritabanı dosya biçimi](https://sqlite.org/fileformat2.html),
[VDBE sanal makine komutları](https://sqlite.org/opcode.html),
[atomik commit işleyişi](https://sqlite.org/atomiccommit.html) ve
[sorgu planlayıcı özeti](https://sqlite.org/optoverview.html) belgeleri
incelenebilir.

### Başlıca C SQLite Referans Dosyaları Eşlemesi

  *  **sqlite.h.in / sqliteInt.h** -> `crates/sqliters-types/` ve
     `crates/sqliters-core/src/lib.rs` (genel tipler, hata kodları, sınırlar,
     bağlantı ve ifade arayüzleri).

  *  **tokenize.c / complete.c / parse.y** -> `crates/sqliters-syntax/`
     (sözcük çözümleme, ifade bütünlüğü denetimi ve SQL ayrıştırma).

  *  **vdbe.c / vdbesort.c / func.c / date.c** -> `crates/sqliters-vdbe/`
     (sanal makine yürütümü, sıralama ve yerleşik SQL fonksiyonları).

  *  **build.c / select.c / insert.c / update.c / delete.c / where.c / fkey.c**
     -> `crates/sqliters-core/` (şema çözümleme, sorgu planlama, baytkod üretimi
     ve kısıtlayıcı denetimi).

  *  **btree.c** -> `crates/sqliters-btree/` (tablo/indeks B-Tree sayfaları,
     taşma zincirleri ve boş sayfa listesi yönetimi).

  *  **pager.c / wal.c** -> `crates/sqliters-pager/` ve
     `crates/sqliters-vfs/src/shm/` (işlemler, *rollback journal* ve WAL-index
     yapıtaşları).

  *  **os_unix.c / os_win.c / memvfs.c** -> `crates/sqliters-vfs/` (işletim
     sistemi dosya G/Ç, kilitleme ve bellek-içi VFS).

  *  **shell.c.in** -> `crates/sqliters-cli/` (komut satırı kabuğu).

  *  **tclsqlite.c / test\*.c / test/\*.test** -> `crates/sqliters-tcltest/`
     ve `tests/` (test altyapısı ve doğrulama paketleri).
