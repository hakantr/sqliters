# V6b.134 — Önceki nullable kayıtlar ve gerçek yazma görüntüsü

ALTER ADD COLUMN öncesindeki kısa kayıtların sona eklenen nullable alanı NULL olarak değerlendirilir. UTF-8, UTF-16LE ve UTF-16BE'de NULL, BLOB, metin, rowid takma adları ve otomatik rowid için 30 C/Rust tam dosya çifti ve 9 rollback SHA eş. Önceki SELECT ve metadata guard'ları korundu. [Nullable ve önceki sürüm kontrolleri](/github/sqliters/target/perf-work/v132-mixed-index-rowid-dml-prototype.OnqsCXT9/sonuc.md)
