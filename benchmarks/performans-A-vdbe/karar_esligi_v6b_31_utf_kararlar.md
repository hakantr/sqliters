# V6b.31 — UTF/NOCASE/sayısal hücrelerin ölçülen C kararları

C sonucu gcov dal numarası ve alınma sayısıdır; kısa devre çıkışları ayrı tutulur. Yazma/dönüşüm sonuçları ana karar tablosunda ve ham farksal çıktılarda gösterilir. Hiç alınmayan yönler ayrı dal dosyasında açık sınıflandırılır.

| Profil / C yeri | C koşulu | C sonucu | Rust yeri |
|---|---|---|---|
| ham hücre / `utf.c:266` | `if( pMem->enc!=SQLITE_UTF8 && desiredEnc!=SQLITE_UTF8 ){` | branch  0 taken 330600 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:266` | `if( pMem->enc!=SQLITE_UTF8 && desiredEnc!=SQLITE_UTF8 ){` | branch  1 taken 141584 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:266` | `if( pMem->enc!=SQLITE_UTF8 && desiredEnc!=SQLITE_UTF8 ){` | branch  2 taken 141072 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:266` | `if( pMem->enc!=SQLITE_UTF8 && desiredEnc!=SQLITE_UTF8 ){` | branch  3 taken 189528 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:270` | `if( rc!=SQLITE_OK ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:270` | `if( rc!=SQLITE_OK ){` | branch  1 taken 141072 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:276` | `while( zIn<zTerm ){` | branch  0 taken 225464 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:276` | `while( zIn<zTerm ){` | branch  1 taken 141072 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:287` | `if( desiredEnc==SQLITE_UTF8 ){` | branch  0 taken 189528 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:287` | `if( desiredEnc==SQLITE_UTF8 ){` | branch  1 taken 141584 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:313` | `if( !zOut ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:313` | `if( !zOut ){` | branch  1 taken 331112 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:318` | `if( pMem->enc==SQLITE_UTF8 ){` | branch  0 taken 141584 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:318` | `if( pMem->enc==SQLITE_UTF8 ){` | branch  1 taken 189528 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:319` | `if( desiredEnc==SQLITE_UTF16LE ){` | branch  0 taken 70792 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:319` | `if( desiredEnc==SQLITE_UTF16LE ){` | branch  1 taken 70792 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:321` | `while( zIn<zTerm ){` | branch  0 taken 216695 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:321` | `while( zIn<zTerm ){` | branch  1 taken 70792 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:322` | `READ_UTF8(zIn, zTerm, c);` | branch  0 taken 57179 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:322` | `READ_UTF8(zIn, zTerm, c);` | branch  1 taken 159516 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:322` | `READ_UTF8(zIn, zTerm, c);` | branch  2 taken 46568 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:322` | `READ_UTF8(zIn, zTerm, c);` | branch  3 taken 22157 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:322` | `READ_UTF8(zIn, zTerm, c);` | branch  4 taken 11546 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:322` | `READ_UTF8(zIn, zTerm, c);` | branch  5 taken 35022 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:322` | `READ_UTF8(zIn, zTerm, c);` | branch  6 taken 8174 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:322` | `READ_UTF8(zIn, zTerm, c);` | branch  7 taken 49005 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:322` | `READ_UTF8(zIn, zTerm, c);` | branch  8 taken 18 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:322` | `READ_UTF8(zIn, zTerm, c);` | branch  9 taken 8156 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:323` | `WRITE_UTF16LE(z, c);` | branch  0 taken 216112 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:323` | `WRITE_UTF16LE(z, c);` | branch  1 taken 583 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:328` | `while( zIn<zTerm ){` | branch  0 taken 216695 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:328` | `while( zIn<zTerm ){` | branch  1 taken 70792 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:329` | `READ_UTF8(zIn, zTerm, c);` | branch  0 taken 57179 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:329` | `READ_UTF8(zIn, zTerm, c);` | branch  1 taken 159516 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:329` | `READ_UTF8(zIn, zTerm, c);` | branch  2 taken 46568 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:329` | `READ_UTF8(zIn, zTerm, c);` | branch  3 taken 22157 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:329` | `READ_UTF8(zIn, zTerm, c);` | branch  4 taken 11546 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:329` | `READ_UTF8(zIn, zTerm, c);` | branch  5 taken 35022 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:329` | `READ_UTF8(zIn, zTerm, c);` | branch  6 taken 8174 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:329` | `READ_UTF8(zIn, zTerm, c);` | branch  7 taken 49005 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:329` | `READ_UTF8(zIn, zTerm, c);` | branch  8 taken 18 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:329` | `READ_UTF8(zIn, zTerm, c);` | branch  9 taken 8156 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:330` | `WRITE_UTF16BE(z, c);` | branch  0 taken 216112 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:330` | `WRITE_UTF16BE(z, c);` | branch  1 taken 583 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:337` | `if( pMem->enc==SQLITE_UTF16LE ){` | branch  0 taken 94764 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:337` | `if( pMem->enc==SQLITE_UTF16LE ){` | branch  1 taken 94764 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:339` | `while( zIn<zTerm ){` | branch  0 taken 159912 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:339` | `while( zIn<zTerm ){` | branch  1 taken 94764 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:342` | `if( c>=0xd800 && c<0xe000 ){` | branch  0 taken 38935 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:342` | `if( c>=0xd800 && c<0xe000 ){` | branch  1 taken 120977 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:342` | `if( c>=0xd800 && c<0xe000 ){` | branch  2 taken 21218 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:342` | `if( c>=0xd800 && c<0xe000 ){` | branch  3 taken 17717 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:357` | `if( zIn<zTerm ){` | branch  0 taken 18680 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:357` | `if( zIn<zTerm ){` | branch  1 taken 2538 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:364` | `WRITE_UTF8(z, c);` | branch  0 taken 2887 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:364` | `WRITE_UTF8(z, c);` | branch  1 taken 157025 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:364` | `WRITE_UTF8(z, c);` | branch  2 taken 4985 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:364` | `WRITE_UTF8(z, c);` | branch  3 taken 152040 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:364` | `WRITE_UTF8(z, c);` | branch  4 taken 133360 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:364` | `WRITE_UTF8(z, c);` | branch  5 taken 18680 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:368` | `while( zIn<zTerm ){` | branch  0 taken 160128 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:368` | `while( zIn<zTerm ){` | branch  1 taken 94764 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:371` | `if( c>=0xd800 && c<0xe000 ){` | branch  0 taken 39073 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:371` | `if( c>=0xd800 && c<0xe000 ){` | branch  1 taken 121055 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:371` | `if( c>=0xd800 && c<0xe000 ){` | branch  2 taken 21072 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:371` | `if( c>=0xd800 && c<0xe000 ){` | branch  3 taken 18001 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:386` | `if( zIn<zTerm ){` | branch  0 taken 18664 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:386` | `if( zIn<zTerm ){` | branch  1 taken 2408 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:393` | `WRITE_UTF8(z, c);` | branch  0 taken 2861 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:393` | `WRITE_UTF8(z, c);` | branch  1 taken 157267 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:393` | `WRITE_UTF8(z, c);` | branch  2 taken 5155 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:393` | `WRITE_UTF8(z, c);` | branch  3 taken 152112 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:393` | `WRITE_UTF8(z, c);` | branch  4 taken 133448 (fallthrough) | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `utf.c:393` | `WRITE_UTF8(z, c);` | branch  5 taken 18664 | `crates/sqliters-vdbe/src/utf.rs:52` |
| ham hücre / `main.c:1124` | `if( 0==r ){` | branch  0 taken 951 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:122` |
| ham hücre / `main.c:1124` | `if( 0==r ){` | branch  1 taken 4932 | `crates/sqliters-vdbe/src/vdbemem.rs:122` |
| ham hücre / `main.c:1091` | `while( nKey1 && pK1[nKey1-1]==' ' ) nKey1--;` | branch  0 taken 5775 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:109` |
| ham hücre / `main.c:1091` | `while( nKey1 && pK1[nKey1-1]==' ' ) nKey1--;` | branch  1 taken 387 | `crates/sqliters-vdbe/src/vdbemem.rs:109` |
| ham hücre / `main.c:1091` | `while( nKey1 && pK1[nKey1-1]==' ' ) nKey1--;` | branch  2 taken 279 | `crates/sqliters-vdbe/src/vdbemem.rs:109` |
| ham hücre / `main.c:1091` | `while( nKey1 && pK1[nKey1-1]==' ' ) nKey1--;` | branch  3 taken 5496 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:109` |
| ham hücre / `main.c:1092` | `while( nKey2 && pK2[nKey2-1]==' ' ) nKey2--;` | branch  0 taken 5786 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:109` |
| ham hücre / `main.c:1092` | `while( nKey2 && pK2[nKey2-1]==' ' ) nKey2--;` | branch  1 taken 377 | `crates/sqliters-vdbe/src/vdbemem.rs:109` |
| ham hücre / `main.c:1092` | `while( nKey2 && pK2[nKey2-1]==' ' ) nKey2--;` | branch  2 taken 280 | `crates/sqliters-vdbe/src/vdbemem.rs:109` |
| ham hücre / `main.c:1092` | `while( nKey2 && pK2[nKey2-1]==' ' ) nKey2--;` | branch  3 taken 5506 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:109` |
| ham hücre / `util.c:444` | `if( zLeft==0 ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:122` |
| ham hücre / `util.c:444` | `if( zLeft==0 ){` | branch  1 taken 5883 | `crates/sqliters-vdbe/src/vdbemem.rs:122` |
| ham hücre / `util.c:445` | `return zRight ? -1 : 0;` | branch  0 never executed (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:122` |
| ham hücre / `util.c:445` | `return zRight ? -1 : 0;` | branch  1 never executed | `crates/sqliters-vdbe/src/vdbemem.rs:122` |
| ham hücre / `util.c:446` | `}else if( zRight==0 ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:122` |
| ham hücre / `util.c:446` | `}else if( zRight==0 ){` | branch  1 taken 5883 | `crates/sqliters-vdbe/src/vdbemem.rs:122` |
| ham hücre / `util.c:451` | `while( N-- > 0 && *a!=0 && UpperToLower[*a]==UpperToLower[*b]){ a++; b++; }` | branch  0 taken 5549 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:122` |
| ham hücre / `util.c:451` | `while( N-- > 0 && *a!=0 && UpperToLower[*a]==UpperToLower[*b]){ a++; b++; }` | branch  1 taken 892 | `crates/sqliters-vdbe/src/vdbemem.rs:122` |
| ham hücre / `util.c:451` | `while( N-- > 0 && *a!=0 && UpperToLower[*a]==UpperToLower[*b]){ a++; b++; }` | branch  2 taken 5383 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:122` |
| ham hücre / `util.c:451` | `while( N-- > 0 && *a!=0 && UpperToLower[*a]==UpperToLower[*b]){ a++; b++; }` | branch  3 taken 166 | `crates/sqliters-vdbe/src/vdbemem.rs:122` |
| ham hücre / `util.c:451` | `while( N-- > 0 && *a!=0 && UpperToLower[*a]==UpperToLower[*b]){ a++; b++; }` | branch  4 taken 558 | `crates/sqliters-vdbe/src/vdbemem.rs:122` |
| ham hücre / `util.c:451` | `while( N-- > 0 && *a!=0 && UpperToLower[*a]==UpperToLower[*b]){ a++; b++; }` | branch  5 taken 4825 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:122` |
| ham hücre / `util.c:452` | `return N<0 ? 0 : UpperToLower[*a] - UpperToLower[*b];` | branch  0 taken 4991 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:122` |
| ham hücre / `util.c:452` | `return N<0 ? 0 : UpperToLower[*a] - UpperToLower[*b];` | branch  1 taken 892 | `crates/sqliters-vdbe/src/vdbemem.rs:122` |
| ham hücre / `util.c:1184` | `if( enc==SQLITE_UTF8 ){` | branch  0 taken 927041 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1184` | `if( enc==SQLITE_UTF8 ){` | branch  1 taken 2116292 | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1190` | `for(i=3-enc; i<length && zNum[i]==0; i+=2){}` | branch  0 taken 15483738 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1190` | `for(i=3-enc; i<length && zNum[i]==0; i+=2){}` | branch  1 taken 8146 | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1190` | `for(i=3-enc; i<length && zNum[i]==0; i+=2){}` | branch  2 taken 13375592 | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1190` | `for(i=3-enc; i<length && zNum[i]==0; i+=2){}` | branch  3 taken 2108146 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1195` | `while( zNum<zEnd && sqlite3Isspace(*zNum) ) zNum+=incr;` | branch  0 taken 2765409 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1195` | `while( zNum<zEnd && sqlite3Isspace(*zNum) ) zNum+=incr;` | branch  1 taken 281366 | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1195` | `while( zNum<zEnd && sqlite3Isspace(*zNum) ) zNum+=incr;` | branch  2 taken 3442 | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1195` | `while( zNum<zEnd && sqlite3Isspace(*zNum) ) zNum+=incr;` | branch  3 taken 2761967 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1196` | `if( zNum<zEnd ){` | branch  0 taken 2761967 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1196` | `if( zNum<zEnd ){` | branch  1 taken 281366 | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1197` | `if( *zNum=='-' ){` | branch  0 taken 394021 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1197` | `if( *zNum=='-' ){` | branch  1 taken 2367946 | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1200` | `}else if( *zNum=='+' ){` | branch  0 taken 558 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1200` | `}else if( *zNum=='+' ){` | branch  1 taken 2367388 | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1205` | `while( zNum<zEnd && zNum[0]=='0' ){ zNum+=incr; } /* Skip leading zeros. */` | branch  0 taken 2762552 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1205` | `while( zNum<zEnd && zNum[0]=='0' ){ zNum+=incr; } /* Skip leading zeros. */` | branch  1 taken 281458 | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1205` | `while( zNum<zEnd && zNum[0]=='0' ){ zNum+=incr; } /* Skip leading zeros. */` | branch  2 taken 677 | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1205` | `while( zNum<zEnd && zNum[0]=='0' ){ zNum+=incr; } /* Skip leading zeros. */` | branch  3 taken 2761875 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1206` | `for(i=0; &zNum[i]<zEnd && (c=(unsigned)zNum[i]-'0')<=9; i+=incr){` | branch  0 taken 19450237 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1206` | `for(i=0; &zNum[i]<zEnd && (c=(unsigned)zNum[i]-'0')<=9; i+=incr){` | branch  1 taken 1588947 | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1206` | `for(i=0; &zNum[i]<zEnd && (c=(unsigned)zNum[i]-'0')<=9; i+=incr){` | branch  2 taken 17995851 | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1206` | `for(i=0; &zNum[i]<zEnd && (c=(unsigned)zNum[i]-'0')<=9; i+=incr){` | branch  3 taken 1454386 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1212` | `if( u>LARGEST_INT64 ){` | branch  0 taken 776216 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1212` | `if( u>LARGEST_INT64 ){` | branch  1 taken 2267117 | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1217` | `*pNum = neg ? SMALLEST_INT64 : LARGEST_INT64;` | branch  0 taken 388108 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1217` | `*pNum = neg ? SMALLEST_INT64 : LARGEST_INT64;` | branch  1 taken 388108 | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1218` | `}else if( neg ){` | branch  0 taken 5913 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1218` | `}else if( neg ){` | branch  1 taken 2261204 | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1224` | `if( i==0 && zStart==zNum ){    /* No digits */` | branch  0 taken 418108 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1224` | `if( i==0 && zStart==zNum ){    /* No digits */` | branch  1 taken 2625225 | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1224` | `if( i==0 && zStart==zNum ){    /* No digits */` | branch  2 taken 417512 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1224` | `if( i==0 && zStart==zNum ){    /* No digits */` | branch  3 taken 596 | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1226` | `}else if( nonNum ){            /* UTF16 with high-order bytes non-zero */` | branch  0 taken 1827850 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1226` | `}else if( nonNum ){            /* UTF16 with high-order bytes non-zero */` | branch  1 taken 797971 | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1228` | `}else if( &zNum[i]<zEnd ){     /* Extra bytes at the end */` | branch  0 taken 796044 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1228` | `}else if( &zNum[i]<zEnd ){     /* Extra bytes at the end */` | branch  1 taken 1927 | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1231` | `if( !sqlite3Isspace(zNum[jj]) ){` | branch  0 taken 794622 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1231` | `if( !sqlite3Isspace(zNum[jj]) ){` | branch  1 taken 16874 | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1236` | `}while( &zNum[jj]<zEnd );` | branch  0 taken 15452 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1236` | `}while( &zNum[jj]<zEnd );` | branch  1 taken 1422 | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1238` | `if( i<19*incr ){` | branch  0 taken 2256428 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1238` | `if( i<19*incr ){` | branch  1 taken 786905 | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1244` | `j = i>19*incr ? 1 : compare2pow63(zNum, incr);` | branch  0 taken 776225 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1244` | `j = i>19*incr ? 1 : compare2pow63(zNum, incr);` | branch  1 taken 10680 | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1245` | `if( j<0 ){` | branch  0 taken 9 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1245` | `if( j<0 ){` | branch  1 taken 786896 | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1250` | `*pNum = neg ? SMALLEST_INT64 : LARGEST_INT64;` | branch  0 taken 393448 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1250` | `*pNum = neg ? SMALLEST_INT64 : LARGEST_INT64;` | branch  1 taken 393448 | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1251` | `if( j>0 ){` | branch  0 taken 10689 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1251` | `if( j>0 ){` | branch  1 taken 776207 | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1258` | `return neg ? rc : 3;` | branch  0 taken 388099 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:1258` | `return neg ? rc : 3;` | branch  1 taken 388108 | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| ham hücre / `util.c:888` | `if( (v = (unsigned)z[0] - '0')<10 ){` | branch  0 taken 3547620 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:888` | `if( (v = (unsigned)z[0] - '0')<10 ){` | branch  1 taken 1220664 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:893` | `while( (v = (unsigned)z[0] - '0')<10 ){` | branch  0 taken 23028498 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:893` | `while( (v = (unsigned)z[0] - '0')<10 ){` | branch  1 taken 2957859 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:896` | `if( s>=(LARGEST_UINT64-9)/10 ){` | branch  0 taken 1179693 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:896` | `if( s>=(LARGEST_UINT64-9)/10 ){` | branch  1 taken 21848805 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:898` | `while( sqlite3Isdigit(z[0]) ){ z++; d++; }` | branch  0 taken 16080 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:898` | `while( sqlite3Isdigit(z[0]) ){ z++; d++; }` | branch  1 taken 1179693 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:902` | `}else if( z[0]=='-' ){` | branch  0 taken 590706 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:902` | `}else if( z[0]=='-' ){` | branch  1 taken 629958 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:905` | `if( (v = (unsigned)z[0] - '0')<10 ) goto parse_integer_part;` | branch  0 taken 589899 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:905` | `if( (v = (unsigned)z[0] - '0')<10 ) goto parse_integer_part;` | branch  1 taken 807 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:906` | `}else if( z[0]=='+' ){` | branch  0 taken 837 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:906` | `}else if( z[0]=='+' ){` | branch  1 taken 629121 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:908` | `if( (v = (unsigned)z[0] - '0')<10 ) goto parse_integer_part;` | branch  0 taken 33 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:908` | `if( (v = (unsigned)z[0] - '0')<10 ) goto parse_integer_part;` | branch  1 taken 804 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:909` | `}else if( sqlite3Isspace(z[0]) ){` | branch  0 taken 5049 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:909` | `}else if( sqlite3Isspace(z[0]) ){` | branch  1 taken 624072 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:910` | `do{ z++; }while( sqlite3Isspace(z[0]) );` | branch  0 taken 114 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:910` | `do{ z++; }while( sqlite3Isspace(z[0]) );` | branch  1 taken 5049 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:917` | `if( *z=='.' ){` | branch  0 taken 594765 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:917` | `if( *z=='.' ){` | branch  1 taken 4168470 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:919` | `if( sqlite3Isdigit(z[0]) ){` | branch  0 taken 590022 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:919` | `if( sqlite3Isdigit(z[0]) ){` | branch  1 taken 4743 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:922` | `if( s<(LARGEST_UINT64-9)/10 ){` | branch  0 taken 598002 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:922` | `if( s<(LARGEST_UINT64-9)/10 ){` | branch  1 taken 60 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:928` | `}while( sqlite3Isdigit(*++z) );` | branch  0 taken 8040 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:928` | `}while( sqlite3Isdigit(*++z) );` | branch  1 taken 590022 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:929` | `}else if( mState==0 ){` | branch  0 taken 837 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:929` | `}else if( mState==0 ){` | branch  1 taken 3906 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:934` | `}else if( mState==0 ){` | branch  0 taken 624816 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:934` | `}else if( mState==0 ){` | branch  1 taken 3543654 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:940` | `if( *z=='e' \|\| *z=='E' ){` | branch  0 taken 3542853 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:940` | `if( *z=='e' \|\| *z=='E' ){` | branch  1 taken 594729 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:940` | `if( *z=='e' \|\| *z=='E' ){` | branch  2 taken 4872 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:940` | `if( *z=='e' \|\| *z=='E' ){` | branch  3 taken 3537981 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:945` | `if( *z=='-' ){` | branch  0 taken 45 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:945` | `if( *z=='-' ){` | branch  1 taken 599556 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:950` | `if( *z=='+' ){` | branch  0 taken 36 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:950` | `if( *z=='+' ){` | branch  1 taken 599520 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:955` | `if( (v = (unsigned)z[0] - '0')<10 ){` | branch  0 taken 590220 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:955` | `if( (v = (unsigned)z[0] - '0')<10 ){` | branch  1 taken 9381 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:959` | `while( (v = (unsigned)z[0] - '0')<10 ){` | branch  0 taken 8094 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:959` | `while( (v = (unsigned)z[0] - '0')<10 ){` | branch  1 taken 590220 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:960` | `exp = exp<10000 ? (exp*10 + v) : 10000;` | branch  0 taken 8094 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:960` | `exp = exp<10000 ? (exp*10 + v) : 10000;` | branch  1 taken 0 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:971` | `if( s==0 ){` | branch  0 taken 900 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:971` | `if( s==0 ){` | branch  1 taken 4136682 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:977` | `if( neg ) *pResult = -*pResult;` | branch  0 taken 589899 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:977` | `if( neg ) *pResult = -*pResult;` | branch  1 taken 3547683 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:981` | `if( z[0]==0 ){` | branch  0 taken 2751489 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:981` | `if( z[0]==0 ){` | branch  1 taken 1386093 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:984` | `if( sqlite3Isspace(z[0]) ){` | branch  0 taken 34059 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:984` | `if( sqlite3Isspace(z[0]) ){` | branch  1 taken 1352034 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:985` | `do{ z++; }while( sqlite3Isspace(*z) );` | branch  0 taken 756 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:985` | `do{ z++; }while( sqlite3Isspace(*z) );` | branch  1 taken 34059 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:986` | `if( z[0]==0 ){` | branch  0 taken 2673 (fallthrough) | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `util.c:986` | `if( z[0]==0 ){` | branch  1 taken 31386 | `crates/sqliters-types/src/fp.rs:469` |
| ham hücre / `vdbeaux.c:4458` | `if( pMem1->enc==pColl->enc ){` | branch  0 taken 9805 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:732` |
| ham hücre / `vdbeaux.c:4458` | `if( pMem1->enc==pColl->enc ){` | branch  1 taken 7844 | `crates/sqliters-vdbe/src/vdbemem.rs:732` |
| ham hücre / `vdbeaux.c:4442` | `if( (v1==0 \|\| v2==0) ){` | branch  0 taken 7844 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:732` |
| ham hücre / `vdbeaux.c:4442` | `if( (v1==0 \|\| v2==0) ){` | branch  1 taken 0 | `crates/sqliters-vdbe/src/vdbemem.rs:732` |
| ham hücre / `vdbeaux.c:4442` | `if( (v1==0 \|\| v2==0) ){` | branch  2 taken 0 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:732` |
| ham hücre / `vdbeaux.c:4442` | `if( (v1==0 \|\| v2==0) ){` | branch  3 taken 7844 | `crates/sqliters-vdbe/src/vdbemem.rs:732` |
| ham hücre / `vdbeaux.c:4443` | `if( prcErr ) *prcErr = SQLITE_NOMEM_BKPT;` | branch  0 never executed (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:732` |
| ham hücre / `vdbeaux.c:4443` | `if( prcErr ) *prcErr = SQLITE_NOMEM_BKPT;` | branch  1 never executed | `crates/sqliters-vdbe/src/vdbemem.rs:732` |
| ham hücre / `vdbemem.c:220` | `if( !(pMem->flags&MEM_Str) ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:276` |
| ham hücre / `vdbemem.c:220` | `if( !(pMem->flags&MEM_Str) ){` | branch  1 taken 15688 | `crates/sqliters-vdbe/src/vdbemem.rs:276` |
| ham hücre / `vdbemem.c:224` | `if( pMem->enc==desiredEnc ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:276` |
| ham hücre / `vdbemem.c:224` | `if( pMem->enc==desiredEnc ){` | branch  1 taken 15688 | `crates/sqliters-vdbe/src/vdbemem.rs:276` |
| ham hücre / `vdbemem.c:647` | `if( flags & (MEM_Int\|MEM_IntReal) ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:285` |
| ham hücre / `vdbemem.c:647` | `if( flags & (MEM_Int\|MEM_IntReal) ){` | branch  1 taken 1587945 | `crates/sqliters-vdbe/src/vdbemem.rs:285` |
| ham hücre / `vdbemem.c:650` | `}else if( flags & MEM_Real ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:285` |
| ham hücre / `vdbemem.c:650` | `}else if( flags & MEM_Real ){` | branch  1 taken 1587945 | `crates/sqliters-vdbe/src/vdbemem.rs:285` |
| ham hücre / `vdbemem.c:652` | `}else if( (flags & (MEM_Str\|MEM_Blob))!=0 && pMem->z!=0 ){` | branch  0 taken 1587945 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:285` |
| ham hücre / `vdbemem.c:652` | `}else if( (flags & (MEM_Str\|MEM_Blob))!=0 && pMem->z!=0 ){` | branch  1 taken 0 | `crates/sqliters-vdbe/src/vdbemem.rs:285` |
| ham hücre / `vdbemem.c:652` | `}else if( (flags & (MEM_Str\|MEM_Blob))!=0 && pMem->z!=0 ){` | branch  2 taken 1587945 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:285` |
| ham hücre / `vdbemem.c:652` | `}else if( (flags & (MEM_Str\|MEM_Blob))!=0 && pMem->z!=0 ){` | branch  3 taken 0 | `crates/sqliters-vdbe/src/vdbemem.rs:285` |
| ham hücre / `vdbemem.c:738` | `if( pMem->z==0 ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:738` | `if( pMem->z==0 ){` | branch  1 taken 4763835 | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:741` | `}else if( pMem->enc==SQLITE_UTF8` | branch  0 taken 1587945 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:741` | `}else if( pMem->enc==SQLITE_UTF8` | branch  1 taken 3175890 | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:742` | `&& ((pMem->flags & MEM_Term)!=0 \|\| sqlite3VdbeMemZeroTerminateIfAble(pMem))` | branch  0 taken 0 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:742` | `&& ((pMem->flags & MEM_Term)!=0 \|\| sqlite3VdbeMemZeroTerminateIfAble(pMem))` | branch  1 taken 1587945 | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:742` | `&& ((pMem->flags & MEM_Term)!=0 \|\| sqlite3VdbeMemZeroTerminateIfAble(pMem))` | branch  3 never executed (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:742` | `&& ((pMem->flags & MEM_Term)!=0 \|\| sqlite3VdbeMemZeroTerminateIfAble(pMem))` | branch  4 never executed | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:745` | `}else if( pMem->n==0 ){` | branch  0 taken 600 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:745` | `}else if( pMem->n==0 ){` | branch  1 taken 3175290 | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:683` | `if( pMem->enc==SQLITE_UTF8 ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:683` | `if( pMem->enc==SQLITE_UTF8 ){` | branch  1 taken 3175290 | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:685` | `if( zCopy ){` | branch  0 never executed (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:685` | `if( zCopy ){` | branch  1 never executed | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:697` | `if( zCopy ){` | branch  0 taken 3175290 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:697` | `if( zCopy ){` | branch  1 taken 0 | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:699` | `if( pMem->enc==SQLITE_UTF16LE ){` | branch  0 taken 1587603 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:699` | `if( pMem->enc==SQLITE_UTF16LE ){` | branch  1 taken 1587687 | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:700` | `for(i=j=0; i<n-1; i+=2, j++){` | branch  0 taken 11614752 | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:700` | `for(i=j=0; i<n-1; i+=2, j++){` | branch  1 taken 6534 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:702` | `if( z[i+1]!=0 ) break;` | branch  0 taken 1581069 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:702` | `if( z[i+1]!=0 ) break;` | branch  1 taken 10033683 | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:705` | `for(i=j=0; i<n-1; i+=2, j++){` | branch  0 taken 11614833 | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:705` | `for(i=j=0; i<n-1; i+=2, j++){` | branch  1 taken 6537 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:706` | `if( z[i]!=0 ) break;` | branch  0 taken 1581150 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:706` | `if( z[i]!=0 ) break;` | branch  1 taken 10033683 | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:713` | `if( i<n ) rc = -100;` | branch  0 taken 3162219 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:713` | `if( i<n ) rc = -100;` | branch  1 taken 13071 | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| ham hücre / `vdbemem.c:899` | `if( (pMem->flags & (MEM_Int\|MEM_Real\|MEM_IntReal\|MEM_Null))==0 ){` | branch  0 taken 1587945 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:402` |
| ham hücre / `vdbemem.c:899` | `if( (pMem->flags & (MEM_Int\|MEM_Real\|MEM_IntReal\|MEM_Null))==0 ){` | branch  1 taken 0 | `crates/sqliters-vdbe/src/vdbemem.rs:402` |
| ham hücre / `vdbemem.c:905` | `if( ((rc&2)==0 && sqlite3Atoi64(pMem->z, &ix, pMem->n, pMem->enc)<2)` | branch  0 taken 1454369 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:402` |
| ham hücre / `vdbemem.c:905` | `if( ((rc&2)==0 && sqlite3Atoi64(pMem->z, &ix, pMem->n, pMem->enc)<2)` | branch  1 taken 133576 | `crates/sqliters-vdbe/src/vdbemem.rs:402` |
| ham hücre / `vdbemem.c:905` | `if( ((rc&2)==0 && sqlite3Atoi64(pMem->z, &ix, pMem->n, pMem->enc)<2)` | branch  3 taken 198899 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:402` |
| ham hücre / `vdbemem.c:905` | `if( ((rc&2)==0 && sqlite3Atoi64(pMem->z, &ix, pMem->n, pMem->enc)<2)` | branch  4 taken 1255470 | `crates/sqliters-vdbe/src/vdbemem.rs:402` |
| ham hücre / `vdbemem.c:906` | `\|\| sqlite3RealSameAsInt(pMem->u.r, (ix = sqlite3RealToI64(pMem->u.r)))` | branch  2 taken 64342 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:402` |
| ham hücre / `vdbemem.c:906` | `\|\| sqlite3RealSameAsInt(pMem->u.r, (ix = sqlite3RealToI64(pMem->u.r)))` | branch  3 taken 268133 | `crates/sqliters-vdbe/src/vdbemem.rs:402` |
| ham hücre / `vdbe.c:359` | `if( rc<=0 ) return;` | branch  0 taken 1583816 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:469` |
| ham hücre / `vdbe.c:359` | `if( rc<=0 ) return;` | branch  1 taken 4129 | `crates/sqliters-vdbe/src/vdbemem.rs:469` |
| ham hücre / `vdbe.c:360` | `if( (rc&2)==0 && alsoAnInt(pRec, rValue, &pRec->u.i) ){` | branch  0 taken 2825 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:469` |
| ham hücre / `vdbe.c:360` | `if( (rc&2)==0 && alsoAnInt(pRec, rValue, &pRec->u.i) ){` | branch  1 taken 1304 | `crates/sqliters-vdbe/src/vdbemem.rs:469` |
| ham hücre / `vdbe.c:360` | `if( (rc&2)==0 && alsoAnInt(pRec, rValue, &pRec->u.i) ){` | branch  3 taken 1860 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:469` |
| ham hücre / `vdbe.c:360` | `if( (rc&2)==0 && alsoAnInt(pRec, rValue, &pRec->u.i) ){` | branch  4 taken 965 | `crates/sqliters-vdbe/src/vdbemem.rs:469` |
| ham hücre / `vdbe.c:365` | `if( bTryForInt ) sqlite3VdbeIntegerAffinity(pRec);` | branch  0 taken 2269 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:469` |
| ham hücre / `vdbe.c:365` | `if( bTryForInt ) sqlite3VdbeIntegerAffinity(pRec);` | branch  1 taken 0 | `crates/sqliters-vdbe/src/vdbemem.rs:469` |
| SQL/API / `vdbemem.c:927` | `if( pMem->flags & MEM_Null ) return SQLITE_OK;` | branch  0 taken 60 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:425` |
| SQL/API / `vdbemem.c:927` | `if( pMem->flags & MEM_Null ) return SQLITE_OK;` | branch  1 taken 364 | `crates/sqliters-vdbe/src/vdbemem.rs:425` |
| SQL/API / `vdbemem.c:928` | `switch( aff ){` | branch  0 taken 36 | `crates/sqliters-vdbe/src/vdbemem.rs:425` |
| SQL/API / `vdbemem.c:928` | `switch( aff ){` | branch  1 taken 84 | `crates/sqliters-vdbe/src/vdbemem.rs:425` |
| SQL/API / `vdbemem.c:928` | `switch( aff ){` | branch  2 taken 84 | `crates/sqliters-vdbe/src/vdbemem.rs:425` |
| SQL/API / `vdbemem.c:928` | `switch( aff ){` | branch  3 taken 84 | `crates/sqliters-vdbe/src/vdbemem.rs:425` |
| SQL/API / `vdbemem.c:928` | `switch( aff ){` | branch  4 taken 76 | `crates/sqliters-vdbe/src/vdbemem.rs:425` |
| SQL/API / `vdbemem.c:930` | `if( (pMem->flags & MEM_Blob)==0 ){` | branch  0 taken 24 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:425` |
| SQL/API / `vdbemem.c:930` | `if( (pMem->flags & MEM_Blob)==0 ){` | branch  1 taken 12 | `crates/sqliters-vdbe/src/vdbemem.rs:425` |
| SQL/API / `vdbemem.c:933` | `if( pMem->flags & MEM_Str ) MemSetTypeFlag(pMem, MEM_Blob);` | branch  0 taken 24 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:425` |
| SQL/API / `vdbemem.c:933` | `if( pMem->flags & MEM_Str ) MemSetTypeFlag(pMem, MEM_Blob);` | branch  1 taken 0 | `crates/sqliters-vdbe/src/vdbemem.rs:425` |
| SQL/API / `vdbemem.c:959` | `if( encoding!=SQLITE_UTF8 ) pMem->n &= ~1;` | branch  0 taken 48 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:425` |
| SQL/API / `vdbemem.c:959` | `if( encoding!=SQLITE_UTF8 ) pMem->n &= ~1;` | branch  1 taken 28 | `crates/sqliters-vdbe/src/vdbemem.rs:425` |
| SQL/API / `vdbemem.c:961` | `if( rc ) return rc;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:425` |
| SQL/API / `vdbemem.c:961` | `if( rc ) return rc;` | branch  1 taken 76 | `crates/sqliters-vdbe/src/vdbemem.rs:425` |
| SQL/API / `vdbe.c:402` | `if( affinity>=SQLITE_AFF_NUMERIC ){` | branch  0 taken 102828 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:493` |
| SQL/API / `vdbe.c:402` | `if( affinity>=SQLITE_AFF_NUMERIC ){` | branch  1 taken 260286 | `crates/sqliters-vdbe/src/vdbemem.rs:493` |
| SQL/API / `vdbe.c:405` | `if( (pRec->flags & MEM_Int)==0 ){ /*OPTIMIZATION-IF-FALSE*/` | branch  0 taken 49104 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:493` |
| SQL/API / `vdbe.c:405` | `if( (pRec->flags & MEM_Int)==0 ){ /*OPTIMIZATION-IF-FALSE*/` | branch  1 taken 53724 | `crates/sqliters-vdbe/src/vdbemem.rs:493` |
| SQL/API / `vdbe.c:406` | `if( (pRec->flags & (MEM_Real\|MEM_IntReal))==0 ){` | branch  0 taken 49104 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:493` |
| SQL/API / `vdbe.c:406` | `if( (pRec->flags & (MEM_Real\|MEM_IntReal))==0 ){` | branch  1 taken 0 | `crates/sqliters-vdbe/src/vdbemem.rs:493` |
| SQL/API / `vdbe.c:407` | `if( pRec->flags & MEM_Str ) applyNumericAffinity(pRec,1);` | branch  0 taken 0 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:493` |
| SQL/API / `vdbe.c:407` | `if( pRec->flags & MEM_Str ) applyNumericAffinity(pRec,1);` | branch  1 taken 49104 | `crates/sqliters-vdbe/src/vdbemem.rs:493` |
| SQL/API / `vdbe.c:408` | `}else if( affinity<=SQLITE_AFF_REAL ){` | branch  0 never executed (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:493` |
| SQL/API / `vdbe.c:408` | `}else if( affinity<=SQLITE_AFF_REAL ){` | branch  1 never executed | `crates/sqliters-vdbe/src/vdbemem.rs:493` |
| SQL/API / `vdbe.c:412` | `}else if( affinity==SQLITE_AFF_TEXT ){` | branch  0 taken 257954 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:493` |
| SQL/API / `vdbe.c:412` | `}else if( affinity==SQLITE_AFF_TEXT ){` | branch  1 taken 2332 | `crates/sqliters-vdbe/src/vdbemem.rs:493` |
| SQL/API / `vdbe.c:418` | `if( 0==(pRec->flags&MEM_Str) ){ /*OPTIMIZATION-IF-FALSE*/` | branch  0 taken 4482 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:493` |
| SQL/API / `vdbe.c:418` | `if( 0==(pRec->flags&MEM_Str) ){ /*OPTIMIZATION-IF-FALSE*/` | branch  1 taken 253472 | `crates/sqliters-vdbe/src/vdbemem.rs:493` |
| SQL/API / `vdbe.c:419` | `if( (pRec->flags&(MEM_Real\|MEM_Int\|MEM_IntReal)) ){` | branch  0 taken 90 (fallthrough) | `crates/sqliters-vdbe/src/vdbemem.rs:493` |
| SQL/API / `vdbe.c:419` | `if( (pRec->flags&(MEM_Real\|MEM_Int\|MEM_IntReal)) ){` | branch  1 taken 4392 | `crates/sqliters-vdbe/src/vdbemem.rs:493` |
