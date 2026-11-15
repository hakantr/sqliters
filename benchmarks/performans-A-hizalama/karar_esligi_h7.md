# H7b — FTS5 decision parity, continued (ext/fts5 3.54.1  <->  crates/sqliters-ext/src/fts5.rs, fts5_index.rs, fts5_index/read.rs, fts5/{expr,main,auxfn,vocab}.rs; host: crates/sqliters-core/src/{engine,vtab,lib}.rs)

This is the H7 document (read side: index reader, expression evaluator, cursor plans, extension API, auxiliary
functions, fts5vocab, host loop) carried forward in full, plus what step H7b added:

- **L** locale: `locale=1` tables, `fts5_locale()` values, `fts5_get_locale()`, the lN columns of `%_content`;
- **S** the storage / index write decisions H7b touched: UPDATE through the saved row (`pSavedRow`), what
  `sqlite3Fts5IndexBeginWrite` and `fts5IndexFlush` leave behind when a flush fails, the totals after a failed
  statement, which messages a failing statement carries (`pConfig->pzErrmsg`);
- **F** the SQL functions FTS5 registers (`fts5_locale`, `fts5_insttoken`, `MATCH`) and their `PRAGMA function_list` rows;
- **H2** host decisions around a virtual-table scan that were missing: IN-list loops, constant WHERE terms, the
  simplified form of OR terms, the 16-constraint omit rule, read-only modules, the RETURNING restriction, the change
  counter after a refused shadow statement.

Speed work of H7b (part 1) changed no decision: the read trace of the 362 H7 scenarios is unchanged (section b).

The Rust functions are ports that keep the C names (`fts5SegIterSeekInit` -> `Fts5Index::seg_iter_seek_init`,
`fts5ExprNodeTest_AND` -> `Fts5Expr::expr_node_test_and`, `fts5FilterMethod` -> `CsrCore::filter`, ...). Each decision
below is a branch condition of the C source; the ones with a trace point emit the same text on both sides: in the
instrumented C amalgamation (`h7/ctrace/patch_h7.py`, class `f5q`, 25 points; write side class `f5`, H5) and in the Rust
source (`sqliters_types::dtrace!`, compiled only with `--cfg sqliters_dtrace`, built by `step5g/tracebuild.sh`).
`rd|k=N` is emitted for EVERY `%_data` block either side fetches, so two equal streams mean: the same blocks, in the
same order, between the same decisions.

## (a) Decision tables (condition -> outcome -> C source)

The function markers were resolved to `file:line` of the function definition in `sqlite/ext/fts5` (3.54.1) by `h7b/mk_karar.py`.
Trace event names (class `f5q`) are given in the last column where the decision has a trace point; a decision without
one is covered only through the events that follow from it (which `%_data` block is fetched next, which rowid the
expression lands on) and through output comparison.

### R1 Structure and block reads
| condition | outcome | source | trace |
|---|---|---|---|
| `p->pStruct==0` | read `%_data` id 10, decode, remember `PRAGMA data_version` in `iStructVersion` | fts5_index.c:1347 `fts5StructureRead` | `struct|read` (+ `rd|k=10`) |
| `p->pStruct!=0` | cached structure, no read | same | (no event) |
| `sqlite3Fts5IndexReset` (xBegin with no open cursor = `fts5NewTransaction`; xRollbackTo): `fts5IndexDataVersion(p)!=p->iStructVersion` | `fts5StructureInvalidate` | fts5_index.c:9584 `sqlite3Fts5IndexReset` | `struct|reset-invalidate` |
| same, versions equal | structure kept | same | (no event) |
| every `fts5DataRead` (leaf, dlidx page, structure, averages, tombstone hash page) | blob read of `%_data` rowid, `p->nRead++` | fts5_index.c:847 `fts5DataRead` | `rd|k=<rowid>` |
| blob missing / too small (`szLeaf` beyond `nn`, `nn<4`) | `FTS5_CORRUPT` ("fts5: corruption found reading blob N from table T" from a vtab method) | fts5_index.c:930 `fts5LeafRead` | (error text) |

### R2 Segment iterator: seek (`fts5SegIterSeekInit`)
| condition | outcome | source | trace |
|---|---|---|---|
| always | `SELECT pgno FROM %_idx WHERE segid=? AND term<=? ORDER BY term DESC LIMIT 1`; `iPg = pgno>>1`, `bDlidx = pgno&1` | fts5_index.c:2710 `fts5SegIterSeekInit` | `idx|seg= pg= dlidx=` |
| `iPg < pSeg->pgnoFirst` | `iPg = pgnoFirst`, `bDlidx = 0` | same | (in `idx`) |
| `iPg >= pgnoFirst` | load that leaf, `fts5LeafSeek(bGe)` | same | `rd` |
| `fts5LeafSeek`: term found on the page (`nMatch==nTerm && nKeep..` exact) | iterator at its doclist | fts5_index.c:2561 `fts5LeafSeek` | (next `rd`) |
| term not on page, `bGe==0` | iterator EOF (leaf released) | same | |
| term not on page, `bGe!=0`, a larger term follows on the page | iterator at that term | same | |
| `bGe!=0`, end of page reached (`bEndOfPage`) | `fts5SegIterNextPage` until a page with a term (`pgidx` non-empty), iterator at its first term; EOF at end of segment | same | `rd` per page |
| prefix-compressed term header runs past `szLeaf`/page (`iOff>=n`, `nKeep>nTerm`...) | `FTS5_CORRUPT` | same | |
| after seek, (`bGe==0` or `SCANONETERM`) and `flags & FTS5INDEX_QUERY_DESC` | `FTS5_SEGITER_REVERSE`; `fts5SegIterReverse` | fts5_index.c:2710 `fts5SegIterSeekInit` | `rev|dl=` |
| after seek, (`bGe==0` or `SCANONETERM`) and `bDlidx` | `fts5SegIterLoadDlidx` (dlidx iterator, `bRev` = reverse flag) | fts5_index.c:2527 `fts5SegIterLoadDlidx` | `dlidx|rev=` + `rd` |
| `bGe==0` or `SCANONETERM` (one term) | `FTS5_SEGITER_ONETERM` | fts5_index.c:2710 `fts5SegIterSeekInit` | |
| pending data and `(flags & SKIPHASH)==0`: exact term (`pTerm` given, not SCAN) | `sqlite3Fts5HashQuery`: copy of the doclist as a one-leaf iterator; miss = iterator EOF | fts5_index.c:2873 `fts5SegIterHashInit` | `hash|scan=0 hit=` |
| pending data, `pTerm==0` or SCAN | `sqlite3Fts5HashScanInit` (sorted entries, size bytes fixed in place) | same | `hash|scan=1 hit=` |

### R3 Segment iterator: forward step (`fts5SegIterNext*`)
| condition | outcome | source | trace |
|---|---|---|---|
| detail=none table | `fts5SegIterNext_None`: rowid deltas only, `nPos=1`, delete flag from the zero bytes | fts5_index.c:2232 `fts5SegIterNext_None` | |
| `FTS5_SEGITER_REVERSE` | `fts5SegIterNext_Reverse` (R4) | fts5_index.c:2198 `fts5SegIterNext_Reverse` | |
| next byte inside the doclist on this leaf (`iOff < n`, delta != 0 or not at a term boundary) | next rowid = previous + delta; `fts5SegIterLoadNPos` | fts5_index.c:2305 `fts5SegIterNext` | |
| `iOff >= pLeaf->szLeaf` (doclist continues on the next leaf) | `fts5SegIterNextPage`; first rowid of that page if it has one (`fts5LeafFirstRowidOff`), else keep paging | same | `rd` |
| a new term starts (`iOff == iEndofDoclist` / pgidx says so) and `ONETERM` | iterator EOF | same | |
| a new term starts, not `ONETERM` (scan) | `fts5SegIterLoadTerm`, `pbNewTerm = 1` | same | |
| hash iterator, entry exhausted | `sqlite3Fts5HashScanNext`; EOF when the scan ends or (`ONETERM`) always | same | |
| position list longer than the rest of the leaf | skipped page by page (`fts5SegIterNextPage`) when the next rowid is needed | same | `rd` |
| leaf chain ends (`iLeafPgno > pSeg->pgnoLast`) | EOF | fts5_index.c:1835 `fts5SegIterNextPage` | |
| tombstone hash page for the rowid present and rowid found (`contentless_delete=1`) | row skipped by the multi-iterator | fts5_index.c:3360 `fts5MultiIterIsDeleted` | `rd` of the tombstone page |

### R4 Segment iterator: reverse (`ORDER BY rowid DESC`)
| condition | outcome | source | trace |
|---|---|---|---|
| doclist ends on the leaf it starts on, no dlidx | `fts5SegIterReverseInitPage`: scan forward recording `aRowidOffset[]`, then step back | fts5_index.c:2436 `fts5SegIterReverse` | `rev|dl=0` |
| dlidx present | `fts5DlidxIterLast` -> last leaf of the doclist (`iLeafPgno`), load it | same | `rev|dl=1`, `rd` |
| no dlidx, doclist spans leaves | walk leaves forward from the term until a leaf with a term (`fts5LeafIsTermless`) or the last page; the last leaf with a rowid is the start | same | `rd` per leaf |
| step: `iRowidOffset > 0` | previous rowid on this page (delta subtracted, offset from `aRowidOffset`) | fts5_index.c:2198 `fts5SegIterNext_Reverse` | |
| step: `iRowidOffset == 0` | `fts5SegIterReverseNewPage`: previous leaf (via `fts5DlidxIterPrev` when there is a dlidx), skipping leaves without a rowid; EOF when `iLeafPgno == iTermLeafPgno` is passed | fts5_index.c:2132 `fts5SegIterReverseNewPage` | `rd` |

### R5 Doclist index (`Fts5DlidxIter`)
| condition | outcome | source | trace |
|---|---|---|---|
| init, forward | level pages read top-down until a level whose first byte says "no child" (`pData->p[0] & 1 == 0`), each `fts5DlidxLvlNext` | fts5_index.c:1775 `fts5DlidxIterInit` | `rd` per level |
| init, reverse | same, then `fts5DlidxIterLast` | same | |
| `fts5DlidxLvlNext`: first call | leaf number + first rowid varints | fts5_index.c:1599 `fts5DlidxLvlNext` | |
| zero bytes follow | each zero = one leaf without a rowid (`iLeafPgno++`) | same | |
| end of the level page | `bEof`; parent level stepped, child page re-read (`fts5DlidxIterNextR`) | fts5_index.c:1631 `fts5DlidxIterNextR` | `rd` |
| `fts5DlidxLvlPrev`: `iOff <= iFirstOff` | `bEof`; parent stepped back, child re-read and positioned on its last entry | fts5_index.c:1702 `fts5DlidxLvlPrev` / fts5_index.c:1735 `fts5DlidxIterPrevR` | `rd` |
| `fts5SegIterNextFrom(iMatch)`: dlidx entry rowid `< iMatch` (forward) / `> iMatch` (reverse) | skip to that leaf (`iLeafPgno` jump, leaf loaded), then step row by row | fts5_index.c:3169 `fts5SegIterNextFrom` | `rd` |
| no dlidx, or first entry already past `iMatch` | step row by row only | same | |

### R6 Multi-iterator (`Fts5Iter` over the segments of a structure)
| condition | outcome | source | trace |
|---|---|---|---|
| `iLevel<0` | one sub-iterator per segment of every level (newest first), plus the hash when `p->pHash` and not `SKIPHASH` | fts5_index.c:3960 `fts5MultiIterNew` | `idx`/`hash` per segment |
| `iLevel>=0` | the `nSegment` oldest segments of that level (merge input; write side) | same | |
| `pTerm==0` | `fts5SegIterInit` (first term of the segment) | same | |
| compare tree: both empty | `aFirst = left` | fts5_index.c:3075 `fts5MultiIterDoCompare` | |
| terms differ | smaller term wins | same | |
| terms equal, rowids differ | smaller rowid wins (`bRev`: larger) | same | |
| terms equal, rowids equal | `bTermEq`; the newer segment wins, the older is advanced (`fts5MultiIterAdvanced`) | same | |
| `bSkipEmpty` and (`nPos==0` or tombstoned) | `fts5MultiIterNext` until a non-empty row | fts5_index.c:3396 `fts5MultiIterNext` | |
| `fts5MultiIterNextFrom(iMatch)` | `fts5MultiIterNext(bFrom=1,iMatch)` until `rowid >= iMatch` (rev: `<=`) or EOF | fts5_index.c:4109 `fts5MultiIterNextFrom` | |
| `fts5MultiIterNew2` (prefix / tokendata doclist held in memory) | one sub-iterator over the buffer | fts5_index.c:4049 `fts5MultiIterNew2` | |

### R7 Output of a row (`xSetOutputs`) and position-list callbacks
| condition | outcome | source | trace |
|---|---|---|---|
| `bNoOutput` (`FTS5INDEX_QUERY_NOOUTPUT`: vocab/integrity scans) | `Noop` | fts5_index.c:3890 `fts5IterSetOutputCb` | |
| detail=none | `None`: rowid, `nData = nPos` | same | |
| detail=full/col, no colset | `Nocolset`: poslist copied (in place if it does not span leaves) | same | |
| colset with 0 columns | `ZeroColset`: `nData = 0` | same | |
| detail=full, colset | `Full`: `fts5IndexExtractColset` / chunked `fts5PoslistFilterCallback` | fts5_index.c:3865 `fts5IterSetOutputs_Full` | |
| detail=col, `nCol<=100` | `Col100`: collist filtered through a bitmap, falls back to `Col` when it spans leaves | fts5_index.c:3822 `fts5IterSetOutputs_Col100` | |
| detail=col, more columns | `Col`: `fts5PoslistOffsetsCallback` | same | |
| poslist spans leaves | `fts5ChunkIterate` reads the following leaves | fts5_index.c:3603 `fts5ChunkIterate` | `rd` |

### R8 `sqlite3Fts5IndexQuery`
| condition | outcome | source | trace |
|---|---|---|---|
| `flags & FTS5INDEX_QUERY_PREFIX`==0 | exact term in the main index (`'0'+term`) | fts5_index.c:7422 `sqlite3Fts5IndexQuery` | `query|flags= idx=0 pidx=0 td=` |
| prefix query, `prefix=` index with exactly `nChar` characters | exact term in that index (`'0'+iIdx`) | same | `query|.. idx=N` |
| prefix query, no index of `nChar` characters but one of `nChar+1` (`nIdxChar==nChar+1`) | `iPrefixIdx` remembered: `fts5SetupPrefixIter` scans that index for the longer terms and adds the exact term of the main index | same | `query|.. pidx=N`, `prefix|idx=N` |
| prefix query, no usable index | `fts5SetupPrefixIter` over the main index | fts5_index.c:6713 `fts5SetupPrefixIter` | `prefix|idx=0 desc= tok=` |
| `tokendata=1`, `iIdx==0` (not a prefix query), flags without `NOTOKENDATA` / `SCAN` | `fts5SetupTokendataIter` (one iterator per token variant `term\0...`, merged by rowid) | fts5_index.c:7280 `fts5SetupTokendataIter` | `query|.. td=1` |
| prefix scan with `bTokendata` and `bPrefixInsttoken` (`fts5_insttoken()` on the query) | token map built while the prefix doclists are merged (`prefixIterSetupTokendataCb`), read by xInstToken (`fts5SetupPrefixIterTokendata` on first use otherwise) | fts5_index.c:6713 `fts5SetupPrefixIter` / fts5_index.c:7590 `fts5SetupPrefixIterTokendata` | `prefix|.. tok=1` |
| prefix scan: term no longer has the prefix | scan ends | fts5_index.c:6362 `fts5VisitEntries` | |
| prefix scan: doclists merged 16 at a time | `fts5MergePrefixLists` (detail!=none: poslists merged per rowid) / `fts5MergeRowidLists` (detail=none) | fts5_index.c:6188 `fts5MergePrefixLists` / fts5_index.c:6103 `fts5MergeRowidLists` | |
| colset given | `fts5IterSetOutputCb` chooses the filter (R7) | fts5_index.c:7422 `sqlite3Fts5IndexQuery` | |
| `sqlite3Fts5IterNextScan` | next row, EOF as soon as `bNewTerm` (vocab: term by term) | fts5_index.c:7539 `sqlite3Fts5IterNextScan` | |
| `sqlite3Fts5IterNextFrom` | tokendata iterator: `fts5TokendataIterNext(bFrom)`, else `fts5MultiIterNextFrom` | fts5_index.c:7563 `sqlite3Fts5IterNextFrom` | |

### E1 Expression construction (`fts5_expr.c`)
| condition | outcome | source |
|---|---|---|
| empty expression / only whitespace | no root node: cursor is EOF without touching the index | fts5_expr.c:271 `sqlite3Fts5ExprNew` |
| `col MATCH x` | root wrapped: colset of that column applied to every nearset (`sqlite3Fts5ParseSetColset`); a nearset that already has a colset gets the intersection, empty intersection = no match | fts5_expr.c:271 `sqlite3Fts5ExprNew` / fts5_expr.c:2193 `fts5ParseSetColset` |
| `-col :` / `- {a b} :` | colset inverted (`sqlite3Fts5ParseColsetInvert`) | fts5_expr.c:2087 `sqlite3Fts5ParseColsetInvert` |
| `{a b}` duplicates / unsorted | colset kept sorted, duplicates dropped | fts5_expr.c:2046 `fts5ParseColset` |
| unknown column name | `no such column: X` | fts5_expr.c:2110 `sqlite3Fts5ParseColset` |
| nearset with one phrase of one term, no synonyms, not `^`-anchored | node `FTS5_TERM` (`xNext = fts5ExprNodeNext_TERM`); otherwise `FTS5_STRING` | fts5_expr.c:2366 `sqlite3Fts5ParseNode` |
| any phrase of the nearset has 0 terms (tokenizer produced nothing) | node `FTS5_EOF` (never matches) | same |
| detail!=full and (`nPhrase!=1`, or the phrase has >1 term, or its first term is `^`-anchored) | `fts5: phrase queries are not supported (detail!=full)` (`NEAR` instead of `phrase` when `nPhrase!=1`) | same |
| `a AND b AND c` | flattened n-ary AND / OR children (`fts5ExprAddChildren`); depth limit `SQLITE_FTS5_MAX_EXPR_DEPTH` | fts5_expr.c:2283 `fts5ExprAddChildren` |
| implicit AND of a multi-token bareword in a trigram/tokenizer split (`bPhraseToAnd`) | phrase turned into AND of terms (`fts5ParsePhraseToAnd`) | fts5_expr.c:2309 `fts5ParsePhraseToAnd` |
| NEAR distance not a number | `expected integer, got "x"` | fts5_expr.c:2010 `sqlite3Fts5ParseSetDistance` |
| token with colocated synonyms (`FTS5_TOKEN_COLOCATED`) | synonym chain on the term | fts5_expr.c:1732 `fts5ParseTokenize` |
| `tokendata=1` | `nQueryTerm` = length up to the first 0x00 | same |
| trigram LIKE / GLOB pattern | `sqlite3Fts5ExprPattern`: AND of the literal runs of >=3 characters; none = full scan (xBestIndex does not use the index then) | fts5_expr.c:364 `sqlite3Fts5ExprPattern` |

### E2 Expression evaluation
| condition | outcome | source | trace |
|---|---|---|---|
| `fts5ExprNodeFirst`: STRING/TERM | every term iterator opened with `sqlite3Fts5IndexQuery(prefix?, desc?, colset)`; any term without rows = node EOF | fts5_expr.c:957 `fts5ExprNearInitAll` | `query` per term |
| AND/OR/NOT | children first, then `fts5ExprNodeTest` | fts5_expr.c:1493 `fts5ExprNodeFirst` | |
| `iFirst` given and root rowid before it | `fts5ExprNodeNext(bFromValid=1, iFirst)` | fts5_expr.c:1552 `sqlite3Fts5ExprFirst` | |
| root on a row the node test rejects (`bNomatch`) | `fts5ExprNodeNext` until a match | same | |
| root rowid beyond `iLast` | root `bEof = 1` | same | |
| TERM node | row = the iterator's row; `bNomatch` when its (colset-filtered) poslist is empty | fts5_expr.c:1229 `fts5ExprNodeTest_TERM` | |
| STRING node: all terms on the same rowid? | advance the laggards with `sqlite3Fts5IterNextFrom` to the largest (desc: smallest) rowid until equal | fts5_expr.c:1094 `fts5ExprNodeTest_STRING` | |
| STRING, same rowid | `fts5ExprNearTest`: single phrase single term = poslist as is; else `fts5ExprPhraseIsMatch` per phrase, then `fts5ExprNearIsMatch` | fts5_expr.c:898 `fts5ExprNearTest` | `phrase|match=`, `near|match=` |
| phrase: positions consecutive (term i at pos+i), first-token anchor `^` (`bFirst`: position 0 of a column) | hit appended to the phrase poslist | fts5_expr.c:605 `fts5ExprPhraseIsMatch` | |
| phrase with synonyms | `fts5ExprSynonymList` merges the synonyms' poslists first | fts5_expr.c:522 `fts5ExprSynonymList` | |
| NEAR: a window of `nNear` + phrase lengths contains every phrase | positions written per phrase | fts5_expr.c:743 `fts5ExprNearIsMatch` | |
| no hit on this row | `bNomatch`: next row of the first term, repeat | fts5_expr.c:1094 `fts5ExprNodeTest_STRING` | |
| AND: children on different rowids | each child advanced `NextFrom` the largest (desc: smallest); any child EOF = node EOF; any child `bNomatch` = `bNomatch` | fts5_expr.c:1332 `fts5ExprNodeTest_AND` | |
| OR | node row = smallest (desc: largest) child rowid; `bNomatch` only if every child at that rowid is | fts5_expr.c:1281 `fts5ExprNodeTest_OR` | |
| OR next | every child on the current rowid (or before `iFrom`) advanced | fts5_expr.c:1300 `fts5ExprNodeNext_OR` | |
| NOT: right child on the same rowid and not `bNomatch` | left advanced | fts5_expr.c:1399 `fts5ExprNodeTest_NOT` | |
| NOT: right child EOF | node follows the left child | same | |

### E3 Position lists the cursor hands out
| condition | outcome | source | trace |
|---|---|---|---|
| phrase's node not on the root rowid, or EOF | empty list (`sqlite3Fts5ExprPoslist`) | fts5_expr.c:2958 `sqlite3Fts5ExprPoslist` | |
| detail!=full, live cursor | `sqlite3Fts5ExprClearPoslists(bLive=1)`: phrases not on the row are `bMiss`; the row text is tokenized and the query terms' positions rebuilt (`sqlite3Fts5ExprPopulatePoslists`) | fts5_expr.c:2987 `sqlite3Fts5ExprClearPoslists` / fts5_expr.c:3084 `sqlite3Fts5ExprPopulatePoslists` | `populate|live=1` |
| detail!=full, sorter row | all phrases cleared, rebuilt from the text, then `sqlite3Fts5ExprCheckPoslists(iRowid)` prunes by node type | fts5_expr.c:3170 `sqlite3Fts5ExprCheckPoslists` | `populate|live=0` |
| populate: token longer than the term and term is a prefix / equal length | position appended (`fts5ExprPopulatePoslistsCb`), colocated tokens keep the position | fts5_expr.c:3038 `fts5ExprPopulatePoslistsCb` | |
| column in the phrase's colset? | columns outside are not tokenized into it (`bOk`) | fts5_expr.c:3084 `sqlite3Fts5ExprPopulatePoslists` | |

### C1 xBestIndex / plan (`fts5BestIndexMethod`)
| condition | outcome | source | trace |
|---|---|---|---|
| `MATCH` on the table column or a text column, usable | idxStr `M<col>` / table `M<nCol>`; `omit` | fts5_main.c:614 `fts5BestIndexMethod` | `vfilter|type= idx=` |
| MATCH that is unusable, or MATCH on the rowid (`iCol<0`) | `SQLITE_CONSTRAINT` (plan rejected) | same | |
| `rank MATCH ?` | idxStr `r` | same | |
| LIKE / GLOB on a column of a trigram table | idxStr `L<col>` / `G<col>` (not omitted) | same | |
| rowid `=` (first one only) | `=`, omitted; cost 25 (25000 with a MATCH); `fts5SetUniqueFlag` | same | |
| no rowid `=`; first rowid `<,<=` and first `>,>=` | `<` / `>` (not omitted: the engine re-tests them); cost 37500 (both) / 50000 (one or none) with MATCH, 750000 / 2250000 / 3000000 without | same | |
| `=` on the table-name column or the rank column (`op==EQ && iCol>=nCol`) | treated exactly like MATCH (`ft = 'x'`, `rank = 'bm25()'`) | same | |
| ORDER BY rank, with MATCH | `FTS5_BI_ORDER_RANK`, `orderByConsumed` | same | |
| ORDER BY rowid [DESC] (a single term; DESC not on a `tokendata=1` table) | `FTS5_BI_ORDER_ROWID` (+`FTS5_BI_ORDER_DESC`), consumed | same | |

### C2 xFilter / xNext / xEof (`fts5FilterMethod` ...)
| condition | outcome | source | trace |
|---|---|---|---|
| MATCH text NULL | cursor EOF | fts5_main.c:1450 `fts5FilterMethod` | `filter|plan= .. eof=1` |
| MATCH text starts with `*` | special query `*id` (cursor id) / `*reads` (`nRead`); other = `unknown special query: X` | fts5_main.c:1155 `fts5SpecialMatch` | `filter|plan=3` |
| several MATCH terms | expressions ANDed (`sqlite3Fts5ExprAnd`) | fts5_main.c:1450 `fts5FilterMethod` | |
| `rank MATCH` value | parsed lazily: `fts5CursorParseRank` at filter only stores name/args; `parse error in rank function: X` at filter when it does not parse | fts5_main.c:1253 `fts5CursorParseRank` | |
| rowid limits from `=,<,>` (in the plan's direction) | `iFirstRowid` / `iLastRowid` | fts5_main.c:1450 `fts5FilterMethod` | |
| the cursor is the inner one of a sorter statement (`pTab->pSortCsr`) | plan `FTS5_PLAN_SOURCE`: shares the outer cursor's expression, `fts5CursorFirst` | same | `filter|plan=2` |
| MATCH, ORDER BY rank | `FTS5_PLAN_SORTED_MATCH`: `fts5CursorFirstSorted` | fts5_main.c:1084 `fts5CursorFirstSorted` | `filter|plan=4`, `sorter|desc=` |
| MATCH otherwise | `FTS5_PLAN_MATCH`: `sqlite3Fts5ExprFirst(iFirst,iLast,bDesc)` | fts5_main.c:1136 `fts5CursorFirst` | `filter|plan=1`, `first|eof=` |
| contentless table without MATCH (`zContent==0`) | `%s: table does not support scanning` | fts5_main.c:1450 `fts5FilterMethod` | |
| rowid `=` (and no MATCH) | `FTS5_PLAN_ROWID`: content statement bound to the rowid | same | `filter|plan=6`, `content` |
| otherwise | `FTS5_PLAN_SCAN`: `SELECT ... FROM content WHERE rowid BETWEEN ? AND ? ORDER BY rowid ASC|DESC` | same | `filter|plan=5` |
| xNext, plan MATCH/SOURCE, `FTS5CSR_REQUIRE_RESEEK` (the table was written while the cursor was open: `fts5TripCursors`) | `fts5CursorReseek`: expression re-run from the current rowid; skip the step if the rowid changed | fts5_main.c:957 `fts5CursorReseek` | `reseek` |
| xNext, plan MATCH/SOURCE | `sqlite3Fts5ExprNext(iLast)`; EOF flag; `fts5CsrNewrow` (REQUIRE_CONTENT/DOCSIZE/INST/POSLIST) | fts5_main.c:991 `fts5NextMethod` | `next|eof=` |
| xNext, SORTED_MATCH | `fts5SorterNext`: next `(rowid, poslist blob)` of the sorter statement; blob split into per-phrase offsets | fts5_main.c:891 `fts5SorterNext` | |
| xNext, SCAN / ROWID | step of the content statement; ROWID plan: EOF after one row | fts5_main.c:991 `fts5NextMethod` | |
| xNext, SPECIAL | EOF | same | |

### C3 xColumn / xRowid / content
| condition | outcome | source | trace |
|---|---|---|---|
| column = table-name column | cursor id (`pCsr->iCsrId`) for plan != SPECIAL; SPECIAL: the special value | fts5_main.c:3050 `fts5ColumnMethod` | |
| column = rank, plan SOURCE | `fts5PoslistBlob` (per-phrase sizes + lists; detail=col: collists; detail=none: empty) | fts5_main.c:2993 `fts5PoslistBlob` | |
| column = rank, MATCH or SORTED_MATCH | rank function called (`fts5FindRankFunction` on first use: `no such function: X`) | fts5_main.c:1202 `fts5FindRankFunction` | `rankfn|name= args=` |
| column = rank, other plans | NULL | fts5_main.c:3050 `fts5ColumnMethod` | |
| text column, contentless (`content=''`) | NULL (no statement) | same | |
| text column, `sqlite3_vtab_nochange` | not read | same | |
| text column otherwise | `fts5SeekCursor(bErrormsg=1)`: content row looked up on first use per row; missing row = `fts5: missing row N from content table 'main'.'t_content'` (SQLITE_CORRUPT_VTAB) | fts5_main.c:1708 `fts5SeekCursor` | `content|rowid=` |
| `fts5SeekCursor` from the API (`bErrormsg=0`) | SQLITE_CORRUPT_VTAB without message ("database disk image is malformed") | same | `content` |
| xRowid: SORTED_MATCH | sorter rowid; MATCH/SOURCE: expression rowid; SCAN/ROWID: statement rowid | fts5_main.c:1664 `fts5CursorRowid` | |

### C4 Sorter statement and rank configuration
| condition | outcome | source | trace |
|---|---|---|---|
| ORDER BY rank | `SELECT rowid, rank FROM %Q.%Q ORDER BY %s(\"%w\"%s%s) ASC|DESC` prepared and stepped with `pSortCsr` set; inner cursor opened by the engine (cursor id consumed) | fts5_main.c:1084 `fts5CursorFirstSorted` | `sorter|desc=`, inner `filter|plan=2` |
| rank function unknown at prepare of the sorter statement | `no such function: X` (statement not stepped) | same | |
| `rank MATCH 'fn(args)'` given | that function / those arguments (`sqlite3Fts5ConfigParseRank`: literals only, `parse error in rank function`) | fts5_main.c:1253 `fts5CursorParseRank` | `rankfn` |
| table option `INSERT INTO t(t, rank) VALUES('rank', ...)` | from `%_config` (`pConfig->zRank/zRankArgs`) | same | |
| neither | `bm25` without arguments | same | |
| rank arguments | evaluated once by `SELECT <args>` into `apRankArg` | same | |

### X1 Extension API (`Fts5ExtensionApi`)
| condition | outcome | source | trace |
|---|---|---|---|
| auxiliary function called with a first argument that is not the id of an open cursor | `no such cursor: N` | fts5_main.c:2936 `fts5ApiCallback` | `aux|fn= plan=` |
| xColumnTotalSize / xRowCount | `%_data` id 1 (averages) read once per `sqlite3Fts5StorageConfigValue`-cache (`bTotalsValid`) | fts5_storage.c:629 `fts5StorageLoadTotals` | `rd|k=1` |
| xColumnSize, `columnsize=1`, first call for the row | `%_docsize` row read (`FTS5CSR_REQUIRE_DOCSIZE`); missing = SQLITE_CORRUPT_VTAB | fts5_main.c:2511 `fts5ApiColumnSize` | `docsize|rowid=` |
| xColumnSize, `columnsize=0`, contentless (`zContent==0`) or `content=` of UNINDEXED kind | -1 for every indexed column | same | |
| xColumnSize, `columnsize=0`, content available | `fts5SeekCursor`, each indexed column tokenized and counted (`fts5ColumnSizeCb`) | same | `content` |
| xInstCount / xInst, first call for the row | `fts5CacheInstArray`: per-phrase readers merged by position (`FTS5CSR_REQUIRE_INST`) | fts5_main.c:2382 `fts5CacheInstArray` | `inst|n=` |
| xPhraseFirst/Next, detail!=full | `fts5CsrPoslist` populates from the text (E3) | fts5_main.c:2312 `fts5CsrPoslist` | `populate` |
| xPhraseFirstColumn, detail=col | collist from the sorter blob (sorted) or the phrase iterator (live) | fts5_main.c:2695 `fts5ApiPhraseFirstColumn` | |
| xQueryPhrase | new cursor (own id) with a clone of the phrase, plan MATCH, stepped to the end calling back per row | fts5_main.c:2862 `fts5ApiQueryPhrase` | `qphrase`, `filter`... |
| xSetAuxdata / xGetAuxdata | per (cursor, function) slot, cleared on the next xFilter (`fts5FreeCursorComponents`) | fts5_main.c:2566 `fts5ApiSetAuxdata` | |
| xColumnText on a contentless table | empty | fts5_main.c:2279 `fts5ApiColumnText` | |
| xColumnLocale | locale of the column value when `locale=1` and the content has one, else NULL | fts5_main.c:2795 `fts5ApiColumnLocale` | |

### X2 Auxiliary functions (`fts5_aux.c`)
| condition | outcome | source |
|---|---|---|
| bm25: first call on the cursor | `nPhrase` IDF values from xQueryPhrase hit counts: `log((nRow - nHit + 0.5)/(nHit + 0.5))`, `<=0` replaced by `1e-6`; `avgdl` = total size / rows; stored as auxdata | fts5_aux.c:630 `fts5Bm25GetData` |
| bm25: per row | per phrase the instances weighted by the column weights (missing weights = 1.0); `score += idf * (freq*(k1+1)) / (freq + k1*(1-b+b*D/avgdl))`; result `-score` | fts5_aux.c:703 `fts5Bm25Function` |
| highlight: wrong argument count | `wrong number of arguments to function highlight()` | fts5_aux.c:243 `fts5HighlightFunction` |
| highlight: column out of range (xColumnText SQLITE_RANGE) | empty string result | same |
| highlight: instances overlapping / adjacent phrases | merged into one open/close pair (`CInstIter`: `iEnd` extended while the next instance starts `<= iEnd`) | fts5_aux.c:54 `fts5CInstIterNext` |
| highlight: text of the row | tokenized, open marker before token `iStart`, close after `iEnd` (`fts5HighlightCb`) | fts5_aux.c:153 `fts5HighlightCb` |
| snippet: wrong argument count (!=5 trailing) | `wrong number of arguments to function snippet()` | fts5_aux.c:428 `fts5SnippetFunction` |
| snippet: `iCol<0` | best column chosen by score; else that column only | same |
| snippet: candidate starts | each instance start, and the sentence starts (`fts5SentenceFinderCb`: token 0, and every token preceded by whitespace that follows `.` or `:`) | fts5_aux.c:338 `fts5SentenceFinderCb` |
| snippet: score | 1000 per distinct phrase, 1 per repeat (`fts5SnippetScore`), +100 at column start, +120 at a sentence start; window shifted left so the hits are centred | fts5_aux.c:371 `fts5SnippetScore` |
| snippet: text before / after the window | ellipsis argument prepended / appended | fts5_aux.c:428 `fts5SnippetFunction` |
| snippet: `nToken` | clamped to 0..64 (`MIN(MAX(n,0),64)`) | same |
| fts5_get_locale: column out of range | `column index out of range` | fts5_aux.c:762 `fts5GetLocaleFunction` |

### V1 fts5vocab cursor (`fts5_vocab.c`)
| condition | outcome | source | trace |
|---|---|---|---|
| xOpen | `SELECT t.%Q FROM %Q.%Q AS t WHERE t.%Q MATCH '*id'` finds the fts5 table's index through the cursor id; not an fts5 table = `no such fts5 table: db.t` | fts5_vocab.c:339 `fts5VocabOpenMethod` | inner `filter|plan=3` |
| xBestIndex: `term =` | idxNum bit 1 (`FTS5_VOCAB_TERM_EQ`), cost 100 | fts5_vocab.c:275 `fts5VocabBestIndexMethod` | `vfilter` |
| `term >=,>` / `<=,<` | bits 2 / 4, cost 500 / 250 (both) | same | |
| ORDER BY term ASC | consumed | same | |
| xFilter | `sqlite3Fts5IndexQuery(term or "", FTS5INDEX_QUERY_SCAN [| NOTOKENDATA])`; `term =`: also the upper bound | fts5_vocab.c:655 `fts5VocabFilterMethod` | `query|flags=..` |
| type row / col, per term | all rows of the term stepped (`sqlite3Fts5IterNextScan`), `doc`/`cnt` summed (col: per column, from collist or poslist; detail=none: only `doc`) | fts5_vocab.c:497 `fts5VocabNextMethod` | `rd` |
| term beyond the `<=` bound (`memcmp` on `nLeTerm` bytes) | EOF | same | |
| col: columns with `doc==0` | skipped | same | |
| type instance | one row per position (`fts5VocabInstanceNext`); detail=col: one per column, offset NULL; detail=none: one per row, col and offset NULL | fts5_vocab.c:465 `fts5VocabInstanceNext` | |
| the fts5 table's structure object is no longer the one the cursor started with (`sqlite3Fts5StructureTest`: `p->pStruct != pCsr->pStruct`) | `SQLITE_ABORT` from xNext | fts5_vocab.c:497 `fts5VocabNextMethod` | |

### H1 Host: how the engine steps a virtual-table scan (vdbe.c / where.c / select.c)
| condition | outcome | source |
|---|---|---|
| `LIMIT 0` | `OP_IfNot` on the limit register jumps to the end of the statement: `OP_VOpen` / `OP_VFilter` are not executed (no cursor id is consumed, a bad MATCH expression is not reported) | select.c `computeLimitRegisters` |
| each row | WHERE terms the plan did not omit are tested one by one in WHERE order (`sqlite3ExprIfFalse`), columns read where the term needs them | where.c `sqlite3WhereCodeOneLoopStart` |
| OFFSET not yet used up | row skipped before any result column is computed | select.c `codeOffset` |
| no sorter (ORDER BY absent or consumed) | result columns left to right, `OP_ResultRow`, LIMIT counter, then `OP_VNext` | select.c `selectInnerLoop` |
| sorter needed | result columns of every row computed and inserted; rows come out after the scan | same |
| function whose first argument is a column of a virtual table, non-aggregate query | `sqlite3VtabOverloadFunction` -> xFindFunction: auxiliary function with the cursor of that row | vtab.c `sqlite3VtabOverloadFunction` |
| same in an aggregate query (argument is `TK_AGG_COLUMN`) | the global stub: `unable to use function X in the requested context` | expr.c / fts5_main.c `fts5ApiCallback` stub |
| error in the middle of the scan | rows already returned stay returned | vdbe.c |
| aggregate without GROUP BY, with ORDER BY | the ORDER BY is not passed to xBestIndex and never evaluated; the columns it names are read on the first row of the loop (`OP_VColumn` behind the first-row flag): `ORDER BY rank` runs the rank function once | select.c `sqlite3Select` (aggregate branch, `sSort.pOrderBy` unused) |
| aggregate without GROUP BY that is a lone `max(X)`, or `min(X)` with X not nullable (the rowid) | `pMinMaxOrderBy` = X [DESC] passed to xBestIndex; when consumed the loop is left after its first row | select.c `minMaxQuery`, where.c `sqlite3WhereMinMaxOptEarlyOut` |
| `min(X)` with a nullable X (every declared column of a virtual table) | `KEYINFO_ORDER_BIGNULL`: no ORDER BY reaches xBestIndex, full scan | select.c `minMaxQuery` |
| aggregate with `LIMIT 0` | `OP_Goto` to the end before `OP_VOpen` | select.c `computeLimitRegisters` |
| `t(args)` in FROM | hidden-column equality terms (`sqlite3WhereTabFuncArgs`); the scan is the same loop | where.c |
| DELETE | the loop reads `OP_VRowid` (and the columns of WHERE terms the plan does not cover), no other column | delete.c |
| `x IN (v1, v2, ...)` offered as an equality and used by the table | one `OP_VFilter` per distinct non-NULL value, ascending (the ephemeral index of the IN list, column affinity applied); the rows of one value are handed out — result columns and auxiliary functions run on the live cursor — before the next value is filtered; ORDER BY is never consumed | where.c `whereLoopAddVirtualOne` (`*pbIn`), wherecode.c `codeEqualityTerm` |
| IN term the table asked to omit (constraint index < 16) | the term is disabled (`disableTerm`): `ft IN ('a','b')` on the table-named column of fts5 is a MATCH per value, `rank IN (...)` a rank setting per value | wherecode.c `sqlite3WhereCodeOneLoopStart` (virtual-table branch) |
| IN term used but not omitted | the term stays a test of every row | same |
| WHERE term without a column of the table (`prereqAll==0`) | tested once before the loop is opened and before the xFilter arguments are computed; false / NULL: no `OP_VFilter` (a MATCH expression with a syntax error is never parsed, `rowid = abs(-9223372036854775808)` never computed) | where.c `sqlite3WhereBegin` ("Special case: a WHERE clause that is constant") |
| OR term of the WHERE clause | analysed in the form `sqlite3ExprSimplifiedAndOr` gives it: an operand that is a 32-bit integer literal / TRUE / FALSE / `x IN ()` is always true or always false; `0 OR x` is x (indexable: `0 OR ft MATCH 'a'` is a MATCH constraint), `x OR 1` is the literal 1 (x is not evaluated), a result that is an AND stays one term (`0 OR (ft MATCH 'a' AND rowid<9)`: MATCH is not offered and fails as a function) | whereexpr.c `exprAnalyze`, expr.c `sqlite3ExprSimplifiedAndOr` |
| the table sets `omit` on a constraint with index >= 16 | not omitted: the term is evaluated by the engine as well (the 17th `ft MATCH ?` term fails with `unable to use function MATCH in the requested context`, also when all 17 terms are equal — omission is per term, not per expression) | where.c `whereLoopAddVirtualOne` (`i<16`) |
| INSERT / UPDATE / DELETE on a table whose module has no xUpdate (fts5vocab, dbstat, eponymous-only modules) | `table X may not be modified`, SQLITE_ERROR when the statement is prepared | delete.c `sqlite3IsReadOnly` / `vtabIsReadOnly` |
| `UPDATE ... RETURNING` / `DELETE ... RETURNING` on a virtual table | `UPDATE RETURNING is not available on virtual tables` (after the table lookup, before column names are resolved; the read-only message wins when both apply) | trigger.c `sqlite3TriggersExist` |
| a statement the module steps on a shadow table (INSERT / REPLACE / DELETE: `changeCntOn`) is refused by `SQLITE_CorruptRdOnly` | it halts with its statement transaction rolled back: `sqlite3VdbeSetChanges(db, 0)` — `changes()` is 0 after the outer statement when that one counts nothing itself (SAVEPOINT, RELEASE, COMMIT, a SELECT on fts5vocab that flushes); an incremental-blob write counts nothing | vdbe.c `OP_Transaction`, vdbeaux.c `sqlite3VdbeHalt` |
| UPDATE of a virtual table: a column the statement does not assign | read with `OPFLAG_NOCHNG`; FTS5's xColumn returns nothing for it (`sqlite3_vtab_nochange`) and its xUpdate takes the value from the saved row, so the scan reads only the columns the WHERE terms and SET expressions name; the hidden columns are read too — for the `rank` column of a MATCH cursor fts5 runs the rank function whatever the flag says (an UPDATE over a row without its `%_docsize` record fails in bm25); R-Tree's xColumn ignores the flag: every column is read (engine: `VTabConfigOption::NochangeUnread`, set by fts5 only) | update.c `updateVirtualTable`, fts5_main.c:3050 `fts5ColumnMethod` |
| a statement returns a row and is stepped again after other statements ran | it continues with the connection's `nChange` / `nTotalChange` / `lastRowid` as they are now | (db fields; the engine kept a per-statement copy) |

### L1 Locale values (`fts5_locale()`, `locale=1`)
| condition | outcome | source |
|---|---|---|
| module registration | 16 header bytes per connection: `sqlite3_randomness` xor the constants `00 D0 C9 65 ...` (`FTS5_LOCALE_HDR`) | fts5_main.c:3763 `fts5Init` |
| `fts5_locale(L, T)`: L NULL or empty | the text of T as a TEXT value (`sqlite3_result_text`; NULL when T is NULL) | fts5_main.c:3648 `fts5LocaleFunc` |
| otherwise | BLOB = header (16 bytes) + locale + 0x00 + text of T (`sqlite3_value_text`; empty for NULL) | same |
| a value is a locale value | BLOB longer than the header whose first 16 bytes are this connection's header | fts5_main.c:1338 `sqlite3Fts5IsLocaleValue` |
| decoding: no 0x00 after the header | SQLITE_MISMATCH | fts5_main.c:1371 `sqlite3Fts5DecodeLocaleValue` |
| INSERT / UPDATE with a locale value into a table without `locale=1` (real rows only, not special inserts) | `fts5_locale() requires locale=1`, SQLITE_MISMATCH | fts5_main.c:1941 `fts5UpdateMethod` |
| `%_content` of a `locale=1` table | text in cN, locale in lN (NULL without one); UNINDEXED columns keep the blob as given | fts5_storage.c:988 `sqlite3Fts5StorageContentInsert` |
| tokenizing a column for INSERT / DELETE / rebuild / integrity-check | the text part is tokenized; the locale is set around the call (`sqlite3Fts5SetLocale`): the built-in tokenizers ignore it | fts5_storage.c:1069 `sqlite3Fts5StorageIndexInsert`, fts5_storage.c:503 `fts5StorageDeleteFromIndex`, fts5_storage.c:869 `sqlite3Fts5StorageRebuild` |
| xColumn of a `locale=1` normal-content table | `fts5_locale(lN, cN)` rebuilt when lN is not NULL; external content: the stored value is decoded when it is a locale value | fts5_main.c:3050 `fts5ColumnMethod`, fts5_main.c:1411 `fts5ExtractExprText` |
| MATCH argument is a locale value | its text part is the query (`fts5ExtractExprText`), the locale is set while the expression is built | fts5_main.c:1450 `fts5FilterMethod` |
| `fts5_get_locale(ft, iCol)` | wrong number of arguments / non-integer column: error text of C; column out of range: SQLITE_RANGE; `locale=0`: NULL; else xColumnLocale (lN of the content row, or the locale part of an external value) | fts5_aux.c:762 `fts5GetLocaleFunction`, fts5_main.c:2795 `fts5ApiColumnLocale` |

### S1 Storage and index write decisions touched in H7b
| condition | outcome | source |
|---|---|---|
| UPDATE, rowid unchanged | `sqlite3Fts5StorageDelete(iOld, 0, bSaveRow=1)`: the row read by FTS5_STMT_LOOKUP2 stays open as `pSavedRow`; then insert | fts5_main.c:1941 `fts5UpdateMethod` |
| column not assigned by the UPDATE (`sqlite3_value_nochange`) with a saved row | value (and lN) of the saved row is written to `%_content` and indexed | fts5_storage.c:988 `sqlite3Fts5StorageContentInsert`, fts5_storage.c:1069 `sqlite3Fts5StorageIndexInsert` |
| same, no saved row (the `%_content` row is missing) | NULL is stored and indexed | same |
| UPDATE that changes the rowid, not OR REPLACE | `sqlite3Fts5StorageFindDeleteRow(iOld)` first (saved row), content insert of the new row, delete of the old, index insert | fts5_main.c:1941 `fts5UpdateMethod` |
| same with OR REPLACE | delete iOld (saved), delete iNew, insert | same |
| contentless table, only UNINDEXED columns assigned | FindDeleteRow + content insert only (`bContent`) | same |
| end of xUpdate | `sqlite3Fts5StorageReleaseDeleteRow` | same |
| `sqlite3Fts5IndexBeginWrite`: flush needed and it fails | `iWriteRowid`, `bDelete`, `nPendingRow` are still updated (the assignments follow `fts5IndexFlush` whatever `p->rc` is) | fts5_index.c:6812 `sqlite3Fts5IndexBeginWrite` |
| `fts5IndexFlush` with `flushRc` set (an earlier flush failed and left pending data) | `p->rc = flushRc`: the bare code, no message again | fts5_index.c:5836 `fts5IndexFlush` |
| flush fails with pending data left | `flushRc = p->rc` (cleared by `fts5IndexDiscardData`) | same |
| xSavepoint / xRelease flush fails | bare code (`pConfig->pzErrmsg` is not set there): `database disk image is malformed` | fts5_main.c:3166 `fts5SavepointMethod`, fts5_main.c:3183 `fts5ReleaseMethod` |
| xUpdate / xFilter / xSync / xColumn fail on a corrupt block | message `fts5: corruption found reading blob N from table "T"` / `fts5: corruption on page P, segment S, table "T"` | fts5_index.c:562 `fts5IndexCorruptRowid`, fts5_index.c:572 `fts5IndexCorruptIter` |
| `sqlite3Fts5StorageIndexInsert` / rebuild loop: BeginWrite or tokenizing fails | `p->nTotalRow++` all the same (not guarded by rc): the in-memory totals keep the row when the transaction goes on | fts5_storage.c:1069 `sqlite3Fts5StorageIndexInsert`, fts5_storage.c:869 `sqlite3Fts5StorageRebuild` |
| `fts5StorageContentlessDelete`: `sqlite3Fts5IndexContentlessDelete` fails | `p->nTotalRow--` all the same | fts5_storage.c:683 `fts5StorageContentlessDelete` |
| `fts5StorageDeleteFromIndex`: rc is an error after the columns | `nTotalRow--` all the same (only `rc==OK && nTotalRow<1` is the corrupt case) | fts5_storage.c:503 `fts5StorageDeleteFromIndex` |

### F1 SQL functions registered by fts5 / rtree (`PRAGMA function_list`)
| function | nArg | flags C reports (mask DETERMINISTIC / DIRECTONLY / SUBTYPE / INNOCUOUS) | source |
|---|---|---|---|
| `fts5_locale` | 2 | 3145728 (INNOCUOUS + RESULT_SUBTYPE shown as SUBTYPE bit) | fts5_main.c:3763 `fts5Init` |
| `fts5_insttoken` | 1 | 2097152 (INNOCUOUS); returns its argument with subtype 73 (`FTS5_INSTTOKEN_SUBTYPE`) | fts5_main.c:3698 `fts5InsttokenFunc` |
| `fts5`, `fts5_source_id` | 1 / 0 | 0 / 2099200 | fts5_main.c:3763 `fts5Init` |
| `bm25`, `highlight`, `snippet`, `fts5_get_locale` (global stubs of the auxiliary functions) | -1 | 0 | fts5_aux.c:806 `sqlite3Fts5AuxInit` |
| `match` | 2 | 0 — `sqlite3_overload_function(db, "MATCH", 2)`: calling it is `unable to use function MATCH in the requested context` | (sqlite3_overload_function) |
| `rtreedepth`, `rtreenode`, `rtreecheck` | 1 / 2 / -1 | 0 | rtree.c `sqlite3RtreeInit` |
| MATCH argument with subtype 73 | `pConfig->bPrefixInsttoken = 1` for this xFilter (restored at its end): prefix scans of the main index of a detail=full table keep the token map (`prefix|tok=1`) | fts5_main.c:1450 `fts5FilterMethod` |



## (b) Decision trace: event-by-event comparison from an empty file (h7/trq.py)

Every scenario is run from an empty database by the instrumented C shell and by the trace build of the engine; the `f5q`
event streams (all fields) and the standard output are compared. Generator: `h7/qfz.py` (tables: detail=full/column/none,
prefix indexes, unicode61 / porter / ascii / trigram tokenizers, contentless, contentless_delete, external content,
columnsize=0, small `pgsz` (doclist indexes), many segments / optimize / merge, pending data inside transactions,
fts5vocab row / col / instance, integrity-check; queries: terms, prefixes, phrases, NEAR, column filters, AND/OR/NOT
nests, `^`, rowid constraints, ORDER BY rowid / rank / bm25, LIMIT / OFFSET, snippet / highlight / bm25 / fts5_get_locale,
rank MATCH, special queries).

| scenario set | scenarios | event streams equal | stdout equal | events C | events Rust |
|---|---|---|---|---|---|
| qfz scenarios, seeds 1-200 (60 queries each) | 200 | 200 | 200 | 2772503 | 2772503 |
| qfz extended scenarios (damaged shadow tables, aux functions in WHERE, ORDER BY bm25, FROM-subqueries, aggregates with ORDER BY / LIMIT, min/max, `*id` / `*reads` probes), seeds 201-360 | 160 | 160 | 160 | 2247471 | 2247471 |
| hand-written scenarios h7/sc/td.sql, h7/sc/mix.sql and the H7b scripts h7b/sc/itok.sql (fts5_insttoken), auxrowid.sql (aux functions on plan ROWID / SCAN), inloop1.sql, inloop2.sql (IN-list loops), constw.sql (constant WHERE terms) | 7 | 7 | 7 | 47108 | 47108 |

No scenario differs.

### Mismatches found by the trace differential while the step was built (all fixed; kept here because each one was a C decision the port did not have)

| found by | C decision | what the engine did | fix |
|---|---|---|---|
| qfz traces | `sqlite3VtabCallCreate` puts the new table in the VTrans list without xBegin | xBegin (-> `sqlite3Fts5IndexReset`) at CREATE VIRTUAL TABLE: an extra `struct\|reset-invalidate` | no xBegin at create |
| qfz traces | result expressions are coded left to right, each reading its columns where it stands | all columns of the row were read first, then the functions ran | one evaluation per result expression (`item_cores`), bm25 at its position |
| qfz traces | `%_idx` lookup is traced after `iPg` is clamped to `pgnoFirst` | traced before | moved |
| qfz traces | OFFSET rows are skipped before any result column is read; `LIMIT 0` skips OP_VOpen | columns read, scan run | `codeOffset` order, no open |
| qfz traces | WHERE terms the plan does not cover are tested one at a time | whole residual WHERE read at once | one probe per term |
| extended set | an empty FROM-subquery is run once | run twice (second time for its column names) | `stmt_tx.empty_sub_cols` |
| h7/sc/td.sql | `ORDER BY rowid DESC` is not consumed on a `tokendata=1` table (`fts5BestIndexMethod`) | consumed: descending iterators (`query flags=2`) | condition added |
| h7/sc/mix.sql | the sorter statement computes the rank function row by row, whatever function it is | functions other than bm25 were computed after the scan (row snapshot) | the single-table fast path evaluates any FTS5 auxiliary function with constant arguments per row |
| `*id` / `*reads` probes | aggregate without GROUP BY: ORDER BY dropped, its columns read on the first row; lone max(X) / min(rowid): order of X requested, loop left after the first row; aggregate with LIMIT 0: no cursor | sorted plan / full scan / scan run | aggregate fast path (see section H1) |
| H7b: f5api seed 21 (trace) | `sqlite3Fts5IndexBeginWrite` sets `iWriteRowid` / `bDelete` / `nPendingRow` after a failed flush too | returned before the assignments: the next statement's flush decision (`begin\|flush=`) differed | assignments moved behind the flush, error returned after them |
| H7b: f5api seeds 21, 184, 372, 872 | `fts5IndexFlush` with `flushRc` set returns the bare code; xSavepoint / xRelease have no `pzErrmsg` | the fts5 message was repeated | `rc_only` at these sites |
| H7b: f5api seeds 142, 173, 405, 713 (gdb on the C driver) | a shadow INSERT / REPLACE / DELETE refused by SQLITE_CorruptRdOnly halts with `sqlite3VdbeSetChanges(db, 0)` | `changes()` kept the previous value | `refuse_write_after_corruption(counted)` stores 0 |
| H7b: f5api seed 224 (trace + source) | `p->nTotalRow++` in `sqlite3Fts5StorageIndexInsert` / rebuild and `nTotalRow--` in `fts5StorageContentlessDelete` are not guarded by rc | the counter was left alone on failure: bm25 values differed later in the same transaction | counters moved in front of the error return |
| H7b: h7b/sc/auxrowid.sql | `snippet()` on a cursor without phrases: `sqlite3_malloc64(0)` is NULL, `rc = SQLITE_NOMEM`; it survives when no column is visited (column out of range) | `column index out of range` | NOMEM start value when `nPhrase==0` |
| H7b: h7b/sc/inloop1.sql (trace) | an IN-list loop hands out the rows of one value before the next xFilter | every xFilter ran first, the rows were kept and auxiliary functions ran on kept rows | `InLoopVtabCursor` (row by row; kept rows only when the engine has to sort through the general path) |
| H7b: h7b/sc/inloop2.sql | an IN term the table asked to omit is disabled | the term was always re-tested: `ft IN ('a','b')` returned no row | omit honoured for IN terms |
| H7b: h7b/sc/constw.sql (trace) | constant WHERE terms are tested before the loop: no xFilter when false / NULL; OR terms in simplified form | xFilter always ran (a bad MATCH expression was reported, a scan was made) | constant-term test in `open_filtered_vtab_cursor_inner`, `expr_simplified_and_or` |
| H7b: h7b/sc/aux2.sql | only the first 16 constraints can be omitted, per term | omission was by expression equality: a 17th equal term was dropped too | one omit entry per term (`strip_omitted_conjuncts` counts) |
| H7b gates (h5 trace suite cfg_cunindexed_*, fz5) | `sqlite3Fts5StorageDelete(.., bSaveRow=1)` on a contentless_delete table with contentless_unindexed=1 looks up the saved row after the delete (`sqlite3Fts5StorageFindDeleteRow`) | my rewrite of the UPDATE arm kept no saved row there: unassigned UNINDEXED columns were written as NULL | saved row read in the contentless-delete branch of `storage_delete` |
| H7b gates (h7b/sc/constw.sql trace, updmatch.sql) | the UPDATE loop reads the hidden columns as well; fts5 runs the rank function for `rank` on a MATCH cursor | my column mask for UPDATE scans left `rank` out: no bm25 call, an UPDATE over a row without its docsize record succeeded | hidden columns are part of the mask |
| H7b gates (rfuzz4, trrt) | R-Tree's xColumn ignores the no-change flag and its xUpdate needs every old value | the UPDATE column mask was applied to every module: coordinates became NULL | mask only for modules that declare `NochangeUnread` (fts5) |

Known trace mismatches that remain (not in the scenario sets above, see "not equal" in the report): an aggregate whose
argument calls an auxiliary function (C fails in the first `AggStep`, the engine reads the scan to its end and then
fails with the same message); a min/max aggregate wrapped in another expression (`max(rank) < 0`: C uses the min/max
plan, the engine scans); scans inside joins; `x IN (SELECT ...)` on a virtual-table column (not offered to xBestIndex:
the engine scans and filters — for the table-named column of fts5 the OUTPUT differs, h7b/sc/known_diff.sql);
auxiliary functions in `UPDATE ... SET` / `DELETE ... WHERE` (computed on kept rows after the scan, output equal,
h7b/sc/known_order.sql); the scan of an UPDATE is read to its end before the WHERE terms the plan does not cover are
tested, so the rank function also runs for rows such a term rejects (h7b/sc/updmatch.sql: `ft MATCH 'hello' AND rowid < 3`
— C tests `rowid < 3` first and never reads `rank` of row 3; output equal here, it would differ if that row had no
`%_docsize` record; gates_f4/trsc_known.txt); a statement over a virtual table that is stepped a few rows at a time while other statements
write (h7b/drv2: C continues the scan and reseeks, the engine has read the scan at the first step — output differs).


## (c) Coverage: times each decision outcome was exercised (C / Rust)

Key = trace event + its outcome fields (`trq.py` OUTCOME table; `query`/`prefix` index numbers folded to 0 / N, `inst` counts to 0 / >0, `vfilter` idxNum to its plan bits).

| decision / outcome | C | Rust |
|---|---|---|
| `aux / fn=bm25 plan=1` | 94087 | 94087 |
| `aux / fn=bm25 plan=2` | 150859 | 150859 |
| `aux / fn=bm25 plan=4` | 57192 | 57192 |
| `aux / fn=bm25 plan=5` | 948 | 948 |
| `aux / fn=bm25 plan=6` | 11 | 11 |
| `aux / fn=fts5_get_locale plan=1` | 210 | 210 |
| `aux / fn=fts5_get_locale plan=6` | 1 | 1 |
| `aux / fn=highlight plan=1` | 64616 | 64616 |
| `aux / fn=highlight plan=2` | 769 | 769 |
| `aux / fn=highlight plan=4` | 13066 | 13066 |
| `aux / fn=highlight plan=5` | 948 | 948 |
| `aux / fn=highlight plan=6` | 16 | 16 |
| `aux / fn=snippet plan=1` | 30133 | 30133 |
| `aux / fn=snippet plan=2` | 1042 | 1042 |
| `aux / fn=snippet plan=4` | 7290 | 7290 |
| `aux / fn=snippet plan=5` | 3 | 3 |
| `aux / fn=snippet plan=6` | 6 | 6 |
| `content / ` | 192706 | 192706 |
| `dlidx / rev=0` | 13723 | 13723 |
| `dlidx / rev=1` | 863 | 863 |
| `docsize / ` | 283645 | 283645 |
| `filter / plan=0 desc=0 err=1 eof=0` | 5378 | 5378 |
| `filter / plan=0 desc=1 err=1 eof=0` | 51 | 51 |
| `filter / plan=1 desc=0 err=0 eof=0` | 6846 | 6846 |
| `filter / plan=1 desc=0 err=0 eof=1` | 2353 | 2353 |
| `filter / plan=1 desc=1 err=0 eof=0` | 2436 | 2436 |
| `filter / plan=1 desc=1 err=0 eof=1` | 687 | 687 |
| `filter / plan=2 desc=0 err=0 eof=0` | 2346 | 2346 |
| `filter / plan=2 desc=0 err=0 eof=1` | 771 | 771 |
| `filter / plan=3 desc=0 err=0 eof=0` | 2198 | 2198 |
| `filter / plan=4 desc=0 err=0 eof=0` | 1979 | 1979 |
| `filter / plan=4 desc=0 err=0 eof=1` | 645 | 645 |
| `filter / plan=4 desc=0 err=1 eof=0` | 38 | 38 |
| `filter / plan=4 desc=1 err=0 eof=0` | 367 | 367 |
| `filter / plan=4 desc=1 err=0 eof=1` | 126 | 126 |
| `filter / plan=4 desc=1 err=1 eof=0` | 6 | 6 |
| `filter / plan=5 desc=0 err=0 eof=0` | 1057 | 1057 |
| `filter / plan=5 desc=0 err=0 eof=1` | 6 | 6 |
| `filter / plan=5 desc=1 err=0 eof=0` | 190 | 190 |
| `filter / plan=6 desc=0 err=0 eof=0` | 2836 | 2836 |
| `filter / plan=6 desc=0 err=0 eof=1` | 36 | 36 |
| `filter / plan=6 desc=1 err=0 eof=0` | 3 | 3 |
| `first / eof=0` | 23076 | 23076 |
| `first / eof=1` | 4713 | 4713 |
| `hash / scan=0 hit=0` | 49556 | 49556 |
| `hash / scan=0 hit=1` | 3680 | 3680 |
| `hash / scan=1 hit=0` | 6244 | 6244 |
| `hash / scan=1 hit=1` | 318 | 318 |
| `idxn / dlidx=0` | 143 | 143 |
| `idxn / dlidx=1` | 24 | 24 |
| `idx / dlidx=0` | 372672 | 372672 |
| `idx / dlidx=1` | 13036 | 13036 |
| `inst / n=0` | 7996 | 7996 |
| `inst / n=>0` | 341084 | 341084 |
| `near / match=0` | 11438 | 11438 |
| `near / match=1` | 11873 | 11873 |
| `next / eof=0` | 1421690 | 1421690 |
| `next / eof=1` | 19733 | 19733 |
| `phrase / match=0` | 140017 | 140017 |
| `phrase / match=1` | 190796 | 190796 |
| `populate / live=0` | 17628 | 17628 |
| `populate / live=1` | 71077 | 71077 |
| `prefix / idx=0 desc=0 tok=0` | 2969 | 2969 |
| `prefix / idx=0 desc=0 tok=1` | 20 | 20 |
| `prefix / idx=0 desc=1 tok=0` | 567 | 567 |
| `prefix / idx=0 desc=1 tok=1` | 3 | 3 |
| `prefix / idx=N desc=0 tok=0` | 541 | 541 |
| `prefix / idx=N desc=1 tok=0` | 89 | 89 |
| `qphrase / ` | 12350 | 12350 |
| `query / flags=0 idx=0 pidx=0 td=0` | 42549 | 42549 |
| `query / flags=0 idx=0 pidx=0 td=1` | 79 | 79 |
| `query / flags=1 idx=N pidx=0 td=0` | 4783 | 4783 |
| `query / flags=1 idx=N pidx=0 td=1` | 21 | 21 |
| `query / flags=1 idx=N pidx=N td=0` | 541 | 541 |
| `query / flags=128 idx=0 pidx=0 td=0` | 348 | 348 |
| `query / flags=2 idx=0 pidx=0 td=0` | 7600 | 7600 |
| `query / flags=3 idx=N pidx=0 td=0` | 864 | 864 |
| `query / flags=3 idx=N pidx=N td=0` | 89 | 89 |
| `query / flags=8 idx=0 pidx=0 td=0` | 1833 | 1833 |
| `rankfn / name=bm25 args=0` | 1519 | 1519 |
| `rankfn / name=bm25 args=1` | 232 | 232 |
| `rankfn / name=highlight args=1` | 14 | 14 |
| `rankfn / name=nosuchfn args=0` | 9 | 9 |
| `rankfn / name=snippet args=1` | 6 | 6 |
| `rd / ` | 1283256 | 1283256 |
| `rev / dl=0` | 22420 | 22420 |
| `rev / dl=1` | 794 | 794 |
| `sorter / desc=0` | 2624 | 2624 |
| `sorter / desc=1` | 493 | 493 |
| `struct / read` | 28610 | 28610 |
| `struct / reset-invalidate` | 395 | 395 |
| `vfilter / type=0 idx=0x200` | 427 | 427 |
| `vfilter / type=1 idx=0x0` | 487 | 487 |
| `vfilter / type=1 idx=0x100` | 117 | 117 |
| `vfilter / type=1 idx=0x200` | 246 | 246 |
| `vfilter / type=1 idx=0x400` | 231 | 231 |
| `vfilter / type=1 idx=0x600` | 190 | 190 |
| `vfilter / type=2 idx=0x100` | 231 | 231 |
| `vfilter / type=2 idx=0x200` | 252 | 252 |

### Outcomes checked against the scenario set

| decision outcome | C | Rust | status |
|---|---|---|---|
| R1 structure read from disk | 28610 | 28610 | exercised, equal counts |
| R1 structure invalidated by sqlite3Fts5IndexReset (data_version changed) | 395 | 395 | exercised, equal counts |
| R2 %_idx lookup, leaf without doclist index | 372672 | 372672 | exercised, equal counts |
| R2 %_idx lookup, leaf with doclist index | 13036 | 13036 | exercised, equal counts |
| R2 fts5SegIterNextInit %_idx lookup (tokendata iterator), no dlidx | 143 | 143 | exercised, equal counts |
| R2 fts5SegIterNextInit, dlidx | 24 | 24 | exercised, equal counts |
| R2 hash: term query hit | 3680 | 3680 | exercised, equal counts |
| R2 hash: term query miss | 49556 | 49556 | exercised, equal counts |
| R2 hash: scan with entries | 318 | 318 | exercised, equal counts |
| R2 hash: scan without entries | 6244 | 6244 | exercised, equal counts |
| R4 reverse iteration without doclist index | 22420 | 22420 | exercised, equal counts |
| R4 reverse iteration with doclist index | 794 | 794 | exercised, equal counts |
| R5 doclist index loaded forward | 13723 | 13723 | exercised, equal counts |
| R5 doclist index loaded reverse | 863 | 863 | exercised, equal counts |
| R8 exact term query | 42549 | 42549 | exercised, equal counts |
| R8 exact term query, descending | 7600 | 7600 | exercised, equal counts |
| R8 prefix query served by a prefix index of that length | 6298 | 6298 | exercised, equal counts |
| R8 prefix query with a prefix index one longer (pidx) | 630 | 630 | exercised, equal counts |
| R8 prefix scan of the main index | 3559 | 3559 | exercised, equal counts |
| R8 prefix scan through a +1 prefix index | 630 | 630 | exercised, equal counts |
| R8 prefix scan descending | 659 | 659 | exercised, equal counts |
| R8 tokendata iterator (fts5SetupTokendataIter) | 100 | 100 | exercised, equal counts |
| R8 prefix scan that keeps the token map (insttoken) | 23 | 23 | exercised, equal counts |
| R8 scan query (fts5vocab / integrity-check) | 2181 | 2181 | exercised, equal counts |
| E2 phrase test: match | 190796 | 190796 | exercised, equal counts |
| E2 phrase test: no match | 140017 | 140017 | exercised, equal counts |
| E2 NEAR test: match | 11873 | 11873 | exercised, equal counts |
| E2 NEAR test: no match | 11438 | 11438 | exercised, equal counts |
| E2 first: row found | 23076 | 23076 | exercised, equal counts |
| E2 first: EOF | 4713 | 4713 | exercised, equal counts |
| C2 next: row | 1421690 | 1421690 | exercised, equal counts |
| C2 next: EOF | 19733 | 19733 | exercised, equal counts |
| C2 reseek after the table was written while the cursor was open (fts5CursorReseek) | 0 | 0 | NOT EXERCISED: not verified by trace |
| E3 poslists rebuilt from the text, live cursor | 71077 | 71077 | exercised, equal counts |
| E3 poslists rebuilt from the text, sorter row | 17628 | 17628 | exercised, equal counts |
| C2 filter: error before a plan is chosen | 5429 | 5429 | exercised, equal counts |
| C2 plan MATCH asc | 9199 | 9199 | exercised, equal counts |
| C2 plan MATCH desc | 3123 | 3123 | exercised, equal counts |
| C2 plan SOURCE (inner cursor of the sorter statement) | 3117 | 3117 | exercised, equal counts |
| C2 plan SPECIAL | 2198 | 2198 | exercised, equal counts |
| C2 plan SORTED_MATCH asc | 2662 | 2662 | exercised, equal counts |
| C2 plan SORTED_MATCH desc | 499 | 499 | exercised, equal counts |
| C2 plan SCAN asc | 1063 | 1063 | exercised, equal counts |
| C2 plan SCAN desc | 190 | 190 | exercised, equal counts |
| C2 plan ROWID | 2875 | 2875 | exercised, equal counts |
| C2 filter ends with an error after the plan was chosen | 44 | 44 | exercised, equal counts |
| C2 filter: cursor EOF at once | 4624 | 4624 | exercised, equal counts |
| C3 content row lookup | 192706 | 192706 | exercised, equal counts |
| X1 %_docsize lookup | 283645 | 283645 | exercised, equal counts |
| C4 sorter statement ascending | 2624 | 2624 | exercised, equal counts |
| C4 sorter statement descending | 493 | 493 | exercised, equal counts |
| C4 rank function bm25 without arguments | 1519 | 1519 | exercised, equal counts |
| C4 rank function with arguments | 252 | 252 | exercised, equal counts |
| C4 rank function other than bm25 | 20 | 20 | exercised, equal counts |
| C4 rank function unknown | 9 | 9 | exercised, equal counts |
| X1 aux function on plan MATCH | 189046 | 189046 | exercised, equal counts |
| X1 aux function on plan SOURCE | 152670 | 152670 | exercised, equal counts |
| X1 aux function on plan SORTED_MATCH | 77548 | 77548 | exercised, equal counts |
| X1 aux function on plan SCAN | 1899 | 1899 | exercised, equal counts |
| X1 aux function on plan ROWID | 34 | 34 | exercised, equal counts |
| X2 bm25 | 303097 | 303097 | exercised, equal counts |
| X2 highlight | 79415 | 79415 | exercised, equal counts |
| X2 snippet | 38474 | 38474 | exercised, equal counts |
| X2 fts5_get_locale | 211 | 211 | exercised, equal counts |
| X1 instance array: no instance | 7996 | 7996 | exercised, equal counts |
| X1 instance array: instances | 341084 | 341084 | exercised, equal counts |
| X1 xQueryPhrase | 12350 | 12350 | exercised, equal counts |
| V1 vocab row | 427 | 427 | exercised, equal counts |
| V1 vocab col | 1271 | 1271 | exercised, equal counts |
| V1 vocab instance | 483 | 483 | exercised, equal counts |
| V1 vocab term =  | 348 | 348 | exercised, equal counts |
| V1 vocab term >=  | 925 | 925 | exercised, equal counts |
| V1 vocab term <=  | 231 | 231 | exercised, equal counts |
| V1 vocab term >= and <= | 190 | 190 | exercised, equal counts |
| V1 vocab full scan | 487 | 487 | exercised, equal counts |

### Outcomes not exercised by the scenario set ("not verified" by trace)

- C2 reseek after the table was written while the cursor was open (fts5CursorReseek)

### Decisions without a trace point (verified by output comparison only, or not verified)

Verified by output only (`h7/qfz.py`: CLI 121,000 queries over 1,000 scenarios, extended phase 94,195 queries over 600
scenarios, API driver 48,400 queries over 400 scenarios; in-crate tests `c_align_h7_tests.rs`):
- E1 expression construction: every parser error message and node shape reaches the output only through the rows / the
  error text of the query.
- C1 `fts5BestIndexMethod` costs: the plan the engine picks is visible through `vfilter|type idx` for fts5vocab and
  through `filter|plan desc` for fts5; the cost numbers themselves were not compared (EXPLAIN QUERY PLAN text is not part
  of the query fuzz).
- X2 the arithmetic of bm25 / the scoring of snippet / the merging of highlight: only the returned values are compared.
- H1 the host loop (LIMIT 0, OFFSET before result columns, WHERE terms one by one, rows before an error): compared
  through which `rd` / `content` / `docsize` / `aux` events appear and through the output.

Verified by output and file bytes only in H7b (no trace point): L1 locale values (h7b/sc/loc1.sql, loc2.sql, loc_ext.sql —
loc2 byte-identical; the files of loc1 / loc_ext hold `fts5_locale()` blobs with the per-connection random header and
cannot be byte-identical, two runs of C differ as well), S1 saved-row UPDATE (upd1.sql, upd2.sql byte-identical), S1
failed-flush decisions (corrupt2.sql, corrupt3.sql byte-identical; f5api 1000 seeds), F1 function list, H2 read-only /
RETURNING (readonly.sql, returning2.sql). In-crate: `c_align_h7b_tests.rs` (17 tests, expectations produced by C).

Reached by a C scenario, NOT reachable in the engine:
- `fts5CursorReseek` (C2) and the SQLITE_ABORT of an open fts5vocab cursor: h7b/drv2 steps a statement a few rows at a
  time while other statements write to the table (API drivers with prepare / step-n / finalize ops). C reaches
  `reseek` 26 to 55 times per scenario. The engine reads a virtual-table scan to its end when the statement is first
  stepped (`RunningExec::MaterializedRead`), so the cursor is closed before the write happens: 0 reseeks, and the rows
  it returns afterwards are the ones of the first step (C returns what the table holds when each row is stepped). The
  port (`CsrCore::cursor_reseek`, `Fts5AuxRegistry::trip_cursors`) has still never run against C.

Not verified (no scenario can reach them with the two shells, or none was written):
- xInstToken (`sqlite3Fts5IterToken`, `fts5TokendataIterAppendMap` consumers): `fts5_insttoken()` and the 'insttoken'
  option now run the prefix scan that keeps the token map (`prefix|tok=1`, 23 events, equal), but no auxiliary function
  of the build calls xInstToken and no built-in tokenizer emits a token with a 0x00 separator.
- synonyms (`FTS5_TOKEN_COLOCATED`: `fts5ExprSynonymList`, the colocated branch of `fts5ExprPopulatePoslistsCb`): no
  built-in tokenizer emits colocated tokens.
- `fts5IterSetOutputs_Col` (detail=column with more than 100 columns), colset intersections with an empty result on
  more than one level.
- a tokenizer that uses the locale (xTokenize v2 `pLocale`): the built-in tokenizers ignore it; only that the locale is
  stored, returned and set around the calls is compared.
- `p->nTotalRow++` after a failed BeginWrite: only through f5api seed 224 (bm25 values later in the transaction); no
  hand-written scenario reaches a failing flush inside the insert half of an UPDATE.
- Shapes whose scan is NOT stepped row by row (the cursor is read to its end at xFilter time): joins, GROUP BY /
  DISTINCT / window queries, aggregates the single-table fast path cannot compile (anything but count / sum / total /
  avg / min / max of a column, COALESCE / IFNULL of those, constants), compound SELECTs, FROM-subqueries, correlated
  subqueries, OR-term (MULTI-INDEX OR) plans, IN-list loops whose rows the general path has to sort, UPDATE scans,
  scans the engine must sort whose result or sort expressions the fast path cannot compile. For them the order of
  events inside a statement can differ from C (which block is read before which function call) and an error half-way
  is reported after the scan.
- Stepped row by row and compared by trace: `SELECT <exprs> FROM ft[(args)] [WHERE ...] [ORDER BY rowid|rank|<expr>]
  [LIMIT/OFFSET]` on one virtual table (fts5, fts5vocab), IN-list loops in that shape, the aggregate forms listed
  above, DELETE scans (rowid only).


## (d) Finder (step5c/run_all.sh over health, t, t2, rt and step5g/sc; baseline dallar/katman_h1/h6b)

```
scenarios before/after/common 245 245 245
WORSE (final or worst-seen): []
final layer improved: 0
worst-seen improved in 0
stdout_equal: regressed [] improved []
stderr_equal: regressed [] improved []
rust_deterministic: regressed [] improved []
before: final {'L0': 241, 'L1': 2, 'L4': 1, 'L6': 1}  worst-seen {'L0': 241, 'L1': 2, 'L4': 1, 'L6': 1}
after: final {'L0': 241, 'L1': 2, 'L4': 1, 'L6': 1}  worst-seen {'L0': 241, 'L1': 2, 'L4': 1, 'L6': 1}
after(all): final {'L0': 241, 'L1': 2, 'L4': 1, 'L6': 1}  worst-seen {'L0': 241, 'L1': 2, 'L4': 1, 'L6': 1}
size changed: []
```

## (e) H7b differential scripts (h7b/sc/*.sql: C shell vs engine, same script from an empty file)

| script | output lines that differ / lines | file |
|---|---|---|
| aux1.sql | 4 / 23 | identical |
| aux2.sql | 0 / 11 | identical |
| auxrowid.sql | 0 / 36 | identical |
| constw.sql | 0 / 45 | identical |
| corrupt2.sql | 0 / 28 | identical |
| corrupt3.sql | 0 / 26 | identical |
| cunindexed.sql | 0 / 18 | identical |
| funclist.sql | 2 / 14 | identical |
| inloop1.sql | 0 / 55 | identical |
| inloop2.sql | 0 / 23 | identical |
| itok.sql | 0 / 80 | identical |
| known_diff.sql | 5 / 5 | identical |
| known_order.sql | 0 / 6 | identical |
| known_returning.sql | 18 / 18 | identical |
| loc1.sql | 0 / 91 | DIFFERENT |
| loc2.sql | 0 / 84 | identical |
| loc_ext.sql | 0 / 8 | DIFFERENT |
| readonly.sql | 0 / 10 | identical |
| returning2.sql | 0 / 12 | identical |
| upd1.sql | 0 / 45 | identical |
| upd2.sql | 0 / 43 | identical |
| updmatch.sql | 0 / 24 | identical |

`known_diff.sql`, `known_order.sql`, `known_returning.sql` hold the statements that are NOT equal to C (kept as records);
`loc1.sql` / `loc_ext.sql` store `fts5_locale()` blobs whose 16-byte header is random per connection: their files cannot be
byte-identical (two runs of C differ too), the output is compared; `aux1.sql` differs in the rows a join returns before an error (join class); `funclist.sql` differs only in
the total number of functions (the C shell registers 55 functions of its own extensions — base64, decimal, ieee754, sha3, regexp, ... — that the engine's shell does not have).

## (f) Interleaved statements (h7b/drv2: prepare / step n rows / finalize while other statements write)

```
reseek_cless: stdout DIFF (355 lines); f5q events DIFF (C 2033 / R 1670); reseek events C 26 / R 0; file identical
reseek_col: stdout DIFF (1123 lines); f5q events DIFF (C 4856 / R 4294); reseek events C 42 / R 0; file identical
reseek_full: stdout DIFF (1196 lines); f5q events DIFF (C 5132 / R 4380); reseek events C 53 / R 0; file identical
reseek_none: stdout DIFF (1086 lines); f5q events DIFF (C 4201 / R 3544); reseek events C 42 / R 0; file identical
reseek_pfx: stdout DIFF (722 lines); f5q events DIFF (C 5365 / R 4353); reseek events C 53 / R 0; file identical
reseek_small: stdout DIFF (3188 lines); f5q events DIFF (C 20975 / R 19579); reseek events C 55 / R 0; file identical
vocab_open: stdout DIFF (51 lines); f5q events DIFF (C 104 / R 84); reseek events C 0 / R 0; file identical
scenarios 7, all-equal 0
```
C reaches `fts5CursorReseek`; the engine does not (see section c).
