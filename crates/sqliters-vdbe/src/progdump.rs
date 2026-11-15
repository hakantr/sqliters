//! Loader of a program dumped from C SQLite's `struct Vdbe` (test oracle of the machine).
//!
//! The dump is written by the C driver of the machine's differential tests right after
//! `sqlite3_prepare_v2()`: every field of the prepared program the machine reads, in a
//! line-oriented lossless text form (strings in hex). Loading it gives the [`Program`] the
//! port of C's code generator would have to produce for the same statement, so the machine
//! can be compared with C on programs the generator does not emit yet (DML, triggers).
//!
//! ```text
//! PROG <id> nOp nMem nCursor nVar usesStmtJournal readOnly bIsReader nResColumn btreeMask lockMask minWriteFileFormat expmask nSub
//! O <opcode name> p1 p2 p3 p5 <p4>      one line per instruction
//! SUB <idx> nOp nMem nCsr [tokenId]     a trigger sub-program, followed by its instructions
//!                                       (tokenId: first sub-program with the same token)
//! END
//! ```
//! `<p4>`: `-` | `S hex` | `C hexname enc` | `I n` | `P idx` | `T hexname` | `X hexname` |
//! `F func` | `G func argc iOp` | `K enc nKey nAll (flags coll)*` | `M value` | `A n i..` |
//! `U selId bComplete hexaff iTable iAddr regReturn` | `V`; `func` = `hexname nArg funcFlags
//! userData xFinalize xValue`; `value` = `N` | `I<int>` | `R<hex bits>` | `T<hex>` | `B<hex>`.

use std::sync::Arc;

use sqliters_types::Value;

use crate::opcodes::{OP_Once, OPCODE_NAME};
use crate::vdbeaux::{
    CollSeq, FuncCtx, FuncDef, KeyInfo, P4, P4Index, P4Table, Program, SubProgram, SubrtnSig,
    VdbeOp,
};

fn unhex(s: &str) -> Result<Vec<u8>, String> {
    if s == "-" {
        return Ok(Vec::new());
    }
    if !s.len().is_multiple_of(2) {
        return Err(format!("odd hex string {s}"));
    }
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

fn text(s: &str) -> Result<String, String> {
    String::from_utf8(unhex(s)?).map_err(|e| e.to_string())
}

fn num<T: std::str::FromStr>(s: Option<&&str>) -> Result<T, String> {
    let s = s.ok_or("missing field")?;
    s.parse::<T>().map_err(|_| format!("bad number {s}"))
}

fn func(f: &[&str]) -> Result<FuncDef, String> {
    Ok(FuncDef {
        name: text(f.first().ok_or("missing function name")?)?,
        n_arg: num(f.get(1))?,
        func_flags: num(f.get(2))?,
        user_data: num(f.get(3))?,
        x_finalize: num::<i32>(f.get(4))? != 0,
        x_value: num::<i32>(f.get(5))? != 0,
    })
}

fn value(s: &str) -> Result<Value, String> {
    let (k, rest) = s.split_at(1);
    Ok(match k {
        "N" => Value::Null,
        "I" => Value::Integer(rest.parse::<i64>().map_err(|e| e.to_string())?),
        "R" => Value::Real(f64::from_bits(
            u64::from_str_radix(rest, 16).map_err(|e| e.to_string())?,
        )),
        "T" => Value::Text(text(rest)?.into()),
        "B" => Value::Blob(unhex(rest)?.into()),
        _ => return Err(format!("bad value {s}")),
    })
}

fn p4(f: &[&str], subs: &[Arc<SubProgram>], tokens: &[u64], cur: usize) -> Result<P4, String> {
    let kind = *f.first().ok_or("missing p4")?;
    let a = &f[1..];
    Ok(match kind {
        "-" => P4::NotUsed,
        "S" => P4::Dynamic(unhex(a.first().ok_or("missing string")?)?),
        "C" => P4::CollSeq(Arc::new(CollSeq {
            name: text(a.first().ok_or("missing collation")?)?,
            enc: num(a.get(1))?,
        })),
        "I" => P4::Int32(num(a.first())?),
        "P" => {
            let i: usize = num(a.first())?;
            if cur != usize::MAX && i <= cur {
                // A recursive trigger: the sub-program names itself (or one that contains
                // it). The reference is a stub (`SubProgram::is_stub`): no instructions,
                // `n_mem == -1`, `n_csr` = index of the sub-program in `Program.p_program`,
                // and its token; the machine runs `Program.p_program[n_csr]`.
                P4::SubProgram(Arc::new(SubProgram {
                    n_mem: -1,
                    n_csr: i as i32,
                    token: *tokens.get(i).ok_or("sub-program index")?,
                    ..SubProgram::default()
                }))
            } else {
                P4::SubProgram(subs.get(i).ok_or("sub-program index")?.clone())
            }
        }
        "T" => P4::Table(Arc::new(P4Table {
            name: text(a.first().ok_or("missing table")?)?,
        })),
        "X" => P4::Index(Arc::new(P4Index {
            name: text(a.first().ok_or("missing index")?)?,
        })),
        "F" => P4::FuncDef(Arc::new(func(a)?)),
        "G" => P4::FuncCtx(Box::new(FuncCtx {
            p_func: Arc::new(func(a)?),
            argc: num(a.get(6))?,
            i_op: num(a.get(7))?,
        })),
        "K" => {
            let enc: u8 = num(a.first())?;
            let n_key: usize = num(a.get(1))?;
            let n_all: usize = num(a.get(2))?;
            let mut k = KeyInfo::alloc(enc, n_key, n_all - n_key);
            for j in 0..n_all {
                k.a_sort_flags[j] = num(a.get(3 + 2 * j))?;
                let c = *a.get(4 + 2 * j).ok_or("missing collation")?;
                if c != "-" {
                    let (name, enc) = c.split_once(':').ok_or("collation without encoding")?;
                    k.a_coll[j] = Some(Arc::new(CollSeq {
                        name: text(name)?,
                        enc: enc.parse::<u8>().map_err(|e| e.to_string())?,
                    }));
                }
            }
            P4::KeyInfo(Arc::new(k))
        }
        "M" => P4::Mem(value(a.first().ok_or("missing value")?)?),
        "A" => {
            let n: usize = num(a.first())?;
            let mut v = Vec::with_capacity(n);
            for j in 0..n {
                v.push(num(a.get(1 + j))?);
            }
            P4::IntArray(v)
        }
        "U" => P4::SubrtnSig(Box::new(SubrtnSig {
            sel_id: num(a.first())?,
            b_complete: num::<i32>(a.get(1))? != 0,
            z_aff: text(a.get(2).ok_or("missing affinity")?)?,
            i_table: num(a.get(3))?,
            i_addr: num(a.get(4))?,
            reg_return: num(a.get(5))?,
        })),
        other => return Err(format!("unsupported p4 kind {other}")),
    })
}

fn op(line: &str, subs: &[Arc<SubProgram>], tokens: &[u64], cur: usize) -> Result<VdbeOp, String> {
    let f: Vec<&str> = line.split(' ').collect();
    if f.first() != Some(&"O") || f.len() < 7 {
        return Err(format!("bad instruction line: {line}"));
    }
    let opcode = OPCODE_NAME
        .iter()
        .position(|n| *n == f[1])
        .ok_or_else(|| format!("unknown opcode {}", f[1]))? as u8;
    Ok(VdbeOp {
        opcode,
        p1: num(f.get(2))?,
        p2: num(f.get(3))?,
        p3: num(f.get(4))?,
        p5: num(f.get(5))?,
        p4: p4(&f[6..], subs, tokens, cur)?,
    })
}

/// The program with id `id` of a dump holding any number of programs, `Ok(None)` when the
/// dump has no program with that id.
pub fn load(dump: &str, id: i64) -> Result<Option<Program>, String> {
    let mut lines = dump.lines();
    let head = loop {
        let Some(l) = lines.next() else {
            return Ok(None);
        };
        if let Some(rest) = l.strip_prefix("PROG ") {
            let f: Vec<&str> = rest.split(' ').collect();
            if num::<i64>(f.first())? == id {
                break f;
            }
        }
    };
    let n_op: usize = num(head.get(1))?;
    let n_sub: usize = num(head.get(13))?;
    let main: Vec<&str> = lines.by_ref().take(n_op).collect();
    // The sub-programs name each other by index. They are built last to first, so a reference
    // to a later one is the sub-program itself; a reference to itself or to an earlier one
    // (recursive triggers) is a token-only stub, see `p4()`.
    let mut raw: Vec<(i32, i32, Vec<&str>)> = Vec::with_capacity(n_sub);
    // `SubProgram.token`: the sub-programs C codes for one trigger (one per ON CONFLICT
    // algorithm) share it; the dump names the first sub-program with the same token.
    let mut tokens: Vec<u64> = Vec::with_capacity(n_sub);
    for i in 0..n_sub {
        let l = lines.next().ok_or("truncated dump")?;
        let f: Vec<&str> = l.split(' ').collect();
        if f.first() != Some(&"SUB") || num::<usize>(f.get(1))? != i {
            return Err(format!("bad sub-program line: {l}"));
        }
        let n: usize = num(f.get(2))?;
        tokens.push(match f.get(5) {
            Some(_) => num::<u64>(f.get(5))? + 1,
            None => i as u64 + 1,
        });
        raw.push((num(f.get(3))?, num(f.get(4))?, lines.by_ref().take(n).collect()));
    }
    let mut subs: Vec<Arc<SubProgram>> = vec![Arc::new(SubProgram::default()); n_sub];
    for i in (0..n_sub).rev() {
        let (n_mem, n_csr, ref ops) = raw[i];
        let a_op = ops.iter().map(|l| op(l, &subs, &tokens, i)).collect::<Result<Vec<_>, _>>()?;
        let n_once = a_op.iter().filter(|o| o.opcode == OP_Once).count() as i32;
        subs[i] = Arc::new(SubProgram {
            a_op,
            n_mem,
            n_csr,
            n_once,
            token: tokens[i],
        });
    }
    let a_op = main.iter().map(|l| op(l, &subs, &tokens, usize::MAX)).collect::<Result<Vec<_>, _>>()?;
    if a_op.len() != n_op {
        return Err("truncated dump".to_owned());
    }
    Ok(Some(Program {
        a_op,
        n_mem: num(head.get(2))?,
        n_cursor: num(head.get(3))?,
        n_var: num(head.get(4))?,
        uses_stmt_journal: num::<i32>(head.get(5))? != 0,
        read_only: num::<i32>(head.get(6))? != 0,
        b_is_reader: num::<i32>(head.get(7))? != 0,
        n_res_column: num(head.get(8))?,
        btree_mask: num(head.get(9))?,
        lock_mask: num(head.get(10))?,
        min_write_file_format: num(head.get(11))?,
        expmask: num(head.get(12))?,
        p_program: subs,
        ..Program::default()
    }))
}

// ---------------------------------------------------------------------------------------------
// Writer (the inverse of `load`): a program in the dump format of the C driver
// ---------------------------------------------------------------------------------------------

fn hexs(b: &[u8]) -> String {
    if b.is_empty() {
        return "-".to_owned();
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn func_text(f: &FuncDef) -> String {
    format!("{} {} {} {} {} {}", hexs(f.name.as_bytes()), f.n_arg, f.func_flags, f.user_data, f.x_finalize as i32, f.x_value as i32)
}

/// The text of one instruction (`O ...` line of `dumpOps()`); `sub_index` numbers the
/// sub-programs.
fn op_text(o: &VdbeOp, sub_index: &mut dyn FnMut(&Arc<SubProgram>) -> usize) -> String {
    let mut s = format!("O {} {} {} {} {} ", OPCODE_NAME[o.opcode as usize], o.p1, o.p2, o.p3, o.p5);
    match &o.p4 {
        P4::NotUsed => s.push('-'),
        P4::Static(z) => {
            s.push_str("S ");
            s.push_str(&hexs(z.as_bytes()));
        }
        P4::Dynamic(z) => {
            s.push_str("S ");
            let n = if o.opcode == crate::opcodes::OP_Blob { (o.p1.max(0) as usize).min(z.len()) } else { z.iter().position(|&c| c == 0).unwrap_or(z.len()) };
            s.push_str(&hexs(&z[..n]));
        }
        P4::CollSeq(c) => s.push_str(&format!("C {} {}", hexs(c.name.as_bytes()), c.enc)),
        P4::Int32(i) => s.push_str(&format!("I {i}")),
        P4::SubProgram(sp) => s.push_str(&format!("P {}", sub_index(sp))),
        P4::Table(t) => s.push_str(&format!("T {}", hexs(t.name.as_bytes()))),
        P4::Index(x) => s.push_str(&format!("X {}", hexs(x.name.as_bytes()))),
        P4::FuncDef(f) => s.push_str(&format!("F {}", func_text(f))),
        P4::FuncCtx(c) => s.push_str(&format!("G {} {} {}", func_text(&c.p_func), c.argc, c.i_op)),
        P4::KeyInfo(k) => {
            s.push_str(&format!("K {} {} {}", k.enc, k.n_key_field, k.n_all_field));
            for j in 0..k.n_all_field as usize {
                let nk = (j as u16) < k.n_key_field;
                s.push_str(&format!(" {} ", if nk { k.a_sort_flags.get(j).copied().unwrap_or(0) } else { 0 }));
                match (nk, k.a_coll.get(j).cloned().flatten()) {
                    (true, Some(c)) => s.push_str(&format!("{}:{}", hexs(c.name.as_bytes()), c.enc)),
                    _ => s.push('-'),
                }
            }
        }
        P4::Mem(v) => {
            s.push_str("M ");
            match v {
                Value::Null => s.push('N'),
                Value::Integer(i) => s.push_str(&format!("I{i}")),
                Value::Real(r) => s.push_str(&format!("R{:016x}", r.to_bits())),
                Value::Text(t) => {
                    s.push('T');
                    s.push_str(&hexs(t.as_bytes()));
                }
                Value::Blob(b) => {
                    s.push('B');
                    s.push_str(&hexs(b));
                }
            }
        }
        P4::IntArray(a) => {
            s.push_str(&format!("A {}", a.len()));
            for x in a {
                s.push_str(&format!(" {x}"));
            }
        }
        P4::SubrtnSig(g) => s.push_str(&format!(
            "U {} {} {} {} {} {}",
            g.sel_id,
            g.b_complete as i32,
            hexs(g.z_aff.as_bytes()),
            g.i_table,
            g.i_addr,
            g.reg_return
        )),
        P4::Vtab(_) => s.push('V'),
    }
    s
}

/// `dumpProgram()` of the C driver: the program `prog` as dump text with id `ln`. Stub
/// sub-program operands stand for `prog.p_program[n_csr]` (C's operand points at that object).
pub fn dump(prog: &Program, ln: i64) -> String {
    let mut subs: Vec<Arc<SubProgram>> = Vec::new();
    let resolve = |sp: &Arc<SubProgram>| -> Arc<SubProgram> {
        if sp.is_stub() { prog.p_program.get(sp.n_csr as usize).cloned().unwrap_or_else(|| sp.clone()) } else { sp.clone() }
    };
    fn index_of(subs: &mut Vec<Arc<SubProgram>>, sp: Arc<SubProgram>) -> usize {
        if let Some(i) = subs.iter().position(|s| Arc::ptr_eq(s, &sp)) {
            return i;
        }
        subs.push(sp);
        subs.len() - 1
    }
    // number the sub-programs first (in the order the instructions name them, nested ones after)
    for o in &prog.a_op {
        if let P4::SubProgram(sp) = &o.p4 {
            index_of(&mut subs, resolve(sp));
        }
    }
    let mut done = 0;
    while done < subs.len() {
        let s = subs[done].clone();
        done += 1;
        for o in &s.a_op {
            if let P4::SubProgram(sp) = &o.p4 {
                index_of(&mut subs, resolve(sp));
            }
        }
    }
    let mut out = format!(
        "PROG {} {} {} {} {} {} {} {} {} {} {} {} {} {}\n",
        ln,
        prog.a_op.len(),
        prog.n_mem,
        prog.n_cursor,
        prog.n_var,
        prog.uses_stmt_journal as i32,
        prog.read_only as i32,
        prog.b_is_reader as i32,
        prog.n_res_column,
        prog.btree_mask,
        prog.lock_mask,
        prog.min_write_file_format,
        prog.expmask,
        subs.len()
    );
    let numbering = subs.clone();
    let mut idx = |sp: &Arc<SubProgram>| -> usize {
        let r = resolve(sp);
        numbering.iter().position(|s| Arc::ptr_eq(s, &r)).unwrap_or(usize::MAX)
    };
    for o in &prog.a_op {
        out.push_str(&op_text(o, &mut idx));
        out.push('\n');
    }
    for (i, s) in numbering.iter().enumerate() {
        let t = numbering.iter().position(|x| x.token == s.token).unwrap_or(i);
        out.push_str(&format!("SUB {} {} {} {} {}\n", i, s.a_op.len(), s.n_mem, s.n_csr, t));
        for o in &s.a_op {
            out.push_str(&op_text(o, &mut idx));
            out.push('\n');
        }
    }
    out.push_str("END\n");
    out
}
