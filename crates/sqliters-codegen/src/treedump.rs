//! A canonical text dump of C's parse tree (`Select` / `Expr` / `ExprList`), used to prove that
//! the tree built by the port of `parse.y` — and the tree after `sqlite3SelectPrep()` — is the
//! tree the reference C library builds. The same dump is produced on the C side by the scratch
//! instrumentation `target/perf-work/v1/tree/patch_tree.py` (`v1DumpSelect`); the two texts are
//! compared byte for byte by `target/perf-work/v1/treediff.py`.

use std::fmt::Write;

use crate::consts::*;
use crate::sqliteint::*;
use crate::tk::*;

fn dump_expr(p: &Parse, e: ExprId, o: &mut String) {
    if e.is_null() {
        o.push('-');
        return;
    }
    let x = p.e(e);
    let _ = write!(o, "({} f={:x}", x.op, x.flags & !EP_Static);
    if x.flags & EP_IntValue != 0 {
        let _ = write!(o, " i={}", x.i_value);
    } else if let Some(t) = &x.z_token {
        let _ = write!(o, " t={}:{}", t.len(), t);
    }
    let _ = write!(o, " a={} o2={}", x.aff_expr, x.op2);
    if (x.flags & EP_TokenOnly) == 0 {
        let _ = write!(o, " h={} L=", x.n_height);
        if x.op == TK_SELECT_COLUMN {
            o.push('^');
        } else {
            dump_expr(p, x.p_left, o);
        }
        o.push_str(" R=");
        dump_expr(p, x.p_right, o);
        o.push_str(" X=");
        if x.flags & EP_xIsSelect != 0 {
            dump_select_x(p, x.x_select(), o);
        } else {
            dump_list(p, x.x_list(), o);
        }
        if (x.flags & EP_Reduced) == 0 {
            let _ = write!(o, " T={} C={} w={}", x.i_table, x.i_column, x.w);
        }
    }
    o.push(')');
}

fn dump_list(p: &Parse, l: ExprListId, o: &mut String) {
    if l.is_null() {
        o.push('-');
        return;
    }
    o.push('[');
    for (i, it) in p.l(l).a.iter().enumerate() {
        if i != 0 {
            o.push(',');
        }
        dump_expr(p, it.p_expr, o);
        if let Some(n) = &it.z_e_name {
            let _ = write!(o, " n={}:{}", n.len(), n);
        }
        let _ = write!(o, " e={} s={} b={} oc={}", it.e_e_name, it.sort_flags, it.b_nulls as i32, it.i_order_by_col);
    }
    o.push(']');
}

fn dump_select_x(p: &Parse, s: SelectId, o: &mut String) {
    if s.is_null() {
        o.push('-');
        return;
    }
    let x = p.s(s);
    let _ = write!(o, "{{op={} sf={:x} id={} E=", x.op, x.sel_flags, x.sel_id);
    dump_list(p, x.p_e_list, o);
    let src = p.src(x.p_src);
    let _ = write!(o, " S={}", if x.p_src.nn() { src.a.len() as i32 } else { -1 });
    for it in &src.a {
        let _ = write!(o, " <sq={} co={} cur={} ", it.is_subquery as i32, it.via_coroutine as i32, it.i_cursor);
        if it.is_subquery
            && let Some(q) = &it.p_subq
        {
            dump_select_x(p, q.p_select, o);
        }
        o.push('>');
    }
    o.push_str(" W=");
    dump_expr(p, x.p_where, o);
    o.push_str(" G=");
    dump_list(p, x.p_group_by, o);
    o.push_str(" H=");
    dump_expr(p, x.p_having, o);
    o.push_str(" O=");
    dump_list(p, x.p_order_by, o);
    o.push_str(" L=");
    dump_expr(p, x.p_limit, o);
    if x.p_with.nn() {
        let w = &p.a_with[x.p_with.ix()];
        let _ = write!(o, " WITH={}", w.a.len());
        for c in &w.a {
            let _ = write!(o, " <{} m={} ", c.z_name, c.e_m10d);
            dump_list(p, c.p_cols, o);
            o.push(' ');
            dump_select_x(p, c.p_select, o);
            o.push('>');
        }
    }
    o.push_str(" P=");
    dump_select_x(p, x.p_prior, o);
    o.push('}');
}

/// `v1DumpSelect(zTag, p)`: record `TREE <tag> <dump>` if tree dumps are requested.
pub fn v1_dump_select(p: &mut Parse, tag: &str, s: SelectId) {
    if p.tree_dump.is_none() {
        return;
    }
    let mut o = format!("TREE {tag} ");
    dump_select_x(p, s, &mut o);
    if let Some(v) = p.tree_dump.as_mut() {
        v.push(o);
    }
}
