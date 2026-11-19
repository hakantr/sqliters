//! Resolve the legacy truth comparison in SQL scopes with known source columns.

use sqliters_syntax::{
    BinaryOp, CommonTableExpr, Expr, FromClause, FunctionArgs, QualifiedName, ResultColumn,
    SelectCore, Stmt, TableOrSubquery, UnaryOp,
};

use crate::db_set::DatabaseSet;

pub(crate) fn resolve_simple_truth_tests(stmt: &mut Stmt, db_set: &DatabaseSet) {
    match stmt {
        Stmt::Select(select) if select.body.compounds.is_empty() => {
            let SelectCore::Select {
                from,
                projection,
                selection,
                group_by,
                having,
                windows,
                ..
            } = &mut select.body.head
            else {
                return;
            };
            if !group_by.is_empty()
                || having.is_some()
                || !windows.is_empty()
                || !select_scope_is_known(from.as_ref(), &select.ctes, db_set)
            {
                return;
            }
            let true_false_alias = projection.iter().any(|column| {
                matches!(
                    column,
                    ResultColumn::Expr { alias: Some(alias), .. } if is_truth_name(alias)
                )
            });
            for column in projection {
                if let ResultColumn::Expr { expr, .. } = column {
                    resolve_expr(expr);
                }
            }
            if !true_false_alias {
                if let Some(predicate) = selection {
                    resolve_expr(predicate);
                }
                for term in &mut select.order_by {
                    resolve_expr(&mut term.expr);
                }
            }
        }
        Stmt::Update(update)
            if update.ctes.is_empty() && update.from.is_none() && update.returning.is_empty() =>
        {
            resolve_write_predicate(&update.table, update.selection.as_mut(), db_set);
        }
        Stmt::Delete(delete) if delete.ctes.is_empty() && delete.returning.is_empty() => {
            resolve_write_predicate(&delete.table, delete.selection.as_mut(), db_set);
        }
        _ => {}
    }
}

fn is_truth_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("true") || name.eq_ignore_ascii_case("false")
}

fn select_scope_is_known(
    from: Option<&FromClause>,
    ctes: &[CommonTableExpr],
    db_set: &DatabaseSet,
) -> bool {
    let Some(from) = from else {
        return true;
    };
    let TableOrSubquery::Table {
        name,
        table_args: None,
        ..
    } = &from.head
    else {
        return false;
    };
    if !from.joins.is_empty() {
        return false;
    }
    if name.schema.is_none()
        && let Some(cte) = ctes
            .iter()
            .find(|cte| cte.name.eq_ignore_ascii_case(&name.name))
    {
        return cte
            .columns
            .as_ref()
            .is_some_and(|columns| columns.iter().all(|column| !is_truth_name(column)));
    }
    table_scope_is_known(name, db_set)
}

fn table_scope_is_known(name: &QualifiedName, db_set: &DatabaseSet) -> bool {
    if let Some((id, table)) = db_set.find_user_table_in_set(name) {
        // Expression and partial indexes need their predicates resolved together with the query.
        return table
            .columns
            .iter()
            .all(|column| !is_truth_name(&column.name))
            && db_set.slot(id).is_some_and(|slot| {
                slot.schema.indexes.iter().all(|index| {
                    !index.table_name.eq_ignore_ascii_case(&table.name)
                        || (index.where_clause.is_none()
                            && index.columns.iter().all(|column| column.expr.is_none()))
                })
            });
    }
    db_set
        .find_connected_vtab_entry_in_set(name, false)
        .is_some_and(|(_, _, entry)| {
            entry
                .declared
                .table_schema
                .columns
                .iter()
                .all(|column| !is_truth_name(&column.name))
        })
}

fn resolve_write_predicate(
    name: &QualifiedName,
    predicate: Option<&mut Expr>,
    db_set: &DatabaseSet,
) {
    if let Some(predicate) = predicate
        && table_scope_is_known(name, db_set)
    {
        resolve_expr(predicate);
    }
}

fn empty_in_left_can_fold(expr: &Expr) -> bool {
    match expr {
        Expr::Unary { expr, .. } | Expr::Collate { expr, .. } | Expr::Cast { expr, .. } => {
            empty_in_left_can_fold(expr)
        }
        Expr::Null
        | Expr::Bool(_)
        | Expr::Integer(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Blob(_)
        | Expr::Bind(_)
        | Expr::Column(_) => true,
        _ => false,
    }
}

fn truth_rhs(expr: &Expr) -> Option<bool> {
    let mut rhs = expr;
    while let Expr::Collate { expr, .. } = rhs {
        rhs = expr;
    }
    match rhs {
        Expr::Bool(value) => Some(*value),
        Expr::InList {
            expr,
            list,
            negated,
        } if list.is_empty() && empty_in_left_can_fold(expr) => Some(*negated),
        _ => None,
    }
}

fn not(expr: Box<Expr>) -> Box<Expr> {
    Box::new(Expr::Unary {
        op: UnaryOp::Not,
        expr,
    })
}

fn scalar_truth_operand(mut expr: &Expr) -> bool {
    while let Expr::Collate { expr: inner, .. }
    | Expr::Unary {
        op: UnaryOp::Plus,
        expr: inner,
    } = expr
    {
        expr = inner;
    }
    !matches!(expr, Expr::RowValue(_) | Expr::Subquery(_))
}

fn resolve_expr(expr: &mut Expr) {
    match expr {
        Expr::Binary { op, left, right } => {
            resolve_expr(left);
            if matches!(op, BinaryOp::Is | BinaryOp::IsNot)
                && scalar_truth_operand(left)
                && let Some(expected) = truth_rhs(right)
            {
                // Compare a normalized integer with 1. NULL remains NULL through NOT, so IS
                // and IS NOT supply SQLite's NULL result. The normalized left expression also
                // prevents the access planners from using this as a point constraint.
                let inner = not(std::mem::replace(left, Box::new(Expr::Null)));
                *left = if expected { not(inner) } else { inner };
                **right = Expr::Integer(1);
            } else {
                resolve_expr(right);
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Collate { expr, .. }
        | Expr::Cast { expr, .. }
        | Expr::IsNull { expr, .. } => resolve_expr(expr),
        Expr::FunctionCall {
            args: FunctionArgs::Exprs(args),
            filter,
            over: None,
            ..
        } => {
            for arg in args {
                resolve_expr(arg);
            }
            if let Some(filter) = filter {
                resolve_expr(filter);
            }
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            if let Some(operand) = operand {
                resolve_expr(operand);
            }
            for (when, then) in branches {
                resolve_expr(when);
                resolve_expr(then);
            }
            if let Some(otherwise) = else_expr {
                resolve_expr(otherwise);
            }
        }
        _ => {}
    }
}
