//! Resolve the legacy truth comparison in SQL scopes with known source columns.

use sqliters_syntax::{
    BinaryOp, ColumnRef, CommonTableExpr, Expr, FromClause, FunctionArgs, QualifiedName,
    ResultColumn, SelectCore, Stmt, TableOrSubquery, UnaryOp,
};

use crate::db_set::DatabaseSet;

#[derive(Clone, Copy, Default)]
struct TruthColumns<'a> {
    true_column: Option<&'a str>,
    false_column: Option<&'a str>,
}

impl<'a> TruthColumns<'a> {
    fn for_keyword(self, value: bool) -> Option<&'a str> {
        if value {
            self.true_column
        } else {
            self.false_column
        }
    }
}

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
            if !group_by.is_empty() || having.is_some() || !windows.is_empty() {
                return;
            }
            let Some(columns) = select_truth_columns(from.as_ref(), &select.ctes, db_set) else {
                return;
            };
            let mut truth_alias = false;
            let mut unshadowed_truth_alias = false;
            for column in projection {
                if let ResultColumn::Expr { expr, alias, .. } = column {
                    if let Some(alias) = alias.as_ref().filter(|alias| is_truth_name(alias)) {
                        truth_alias = true;
                        unshadowed_truth_alias |= columns
                            .for_keyword(alias.eq_ignore_ascii_case("true"))
                            .is_none();
                    }
                    resolve_expr(expr, columns);
                }
            }
            // WHERE resolves a source column before consulting a result alias.
            if !unshadowed_truth_alias && let Some(predicate) = selection {
                resolve_expr(predicate, columns);
            }
            if !truth_alias {
                for term in &mut select.order_by {
                    resolve_expr(&mut term.expr, columns);
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

fn select_truth_columns<'a>(
    from: Option<&FromClause>,
    ctes: &[CommonTableExpr],
    db_set: &'a DatabaseSet,
) -> Option<TruthColumns<'a>> {
    let Some(from) = from else {
        return Some(TruthColumns::default());
    };
    let TableOrSubquery::Table {
        name,
        table_args: None,
        ..
    } = &from.head
    else {
        return None;
    };
    if !from.joins.is_empty() {
        return None;
    }
    if name.schema.is_none()
        && let Some(cte) = ctes
            .iter()
            .find(|cte| cte.name.eq_ignore_ascii_case(&name.name))
    {
        let columns = cte.columns.as_ref()?;
        return columns
            .iter()
            .all(|column| !is_truth_name(column))
            .then(TruthColumns::default);
    }
    table_truth_columns(name, db_set)
}

fn table_truth_columns<'a>(
    name: &QualifiedName,
    db_set: &'a DatabaseSet,
) -> Option<TruthColumns<'a>> {
    if let Some((id, table)) = db_set.find_user_table_in_set(name) {
        // Expression and partial indexes need their predicates resolved together with the query.
        let slot = db_set.slot(id)?;
        if slot.schema.indexes.iter().any(|index| {
            index.table_name.eq_ignore_ascii_case(&table.name)
                && (index.where_clause.is_some()
                    || index.columns.iter().any(|column| column.expr.is_some()))
        }) {
            return None;
        }
        return Some(TruthColumns {
            true_column: table
                .find_column("true")
                .map(|(_, column)| column.name.as_str()),
            false_column: table
                .find_column("false")
                .map(|(_, column)| column.name.as_str()),
        });
    }
    db_set
        .find_connected_vtab_entry_in_set(name, false)
        .filter(|(_, _, entry)| {
            entry
                .declared
                .table_schema
                .columns
                .iter()
                .all(|column| !is_truth_name(&column.name))
        })
        .map(|_| TruthColumns::default())
}

fn resolve_write_predicate(
    name: &QualifiedName,
    predicate: Option<&mut Expr>,
    db_set: &DatabaseSet,
) {
    if let Some(predicate) = predicate
        && let Some(columns) = table_truth_columns(name, db_set)
    {
        resolve_expr(predicate, columns);
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

fn resolve_expr(expr: &mut Expr, columns: TruthColumns<'_>) {
    match expr {
        Expr::Bool(value) => {
            if let Some(name) = columns.for_keyword(*value) {
                *expr = Expr::Column(ColumnRef::unqualified(name));
            }
        }
        Expr::Binary { op, left, right } => {
            resolve_expr(left, columns);
            resolve_expr(right, columns);
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
            }
        }
        Expr::Unary { expr, .. }
        | Expr::Collate { expr, .. }
        | Expr::Cast { expr, .. }
        | Expr::IsNull { expr, .. } => resolve_expr(expr, columns),
        Expr::FunctionCall {
            args: FunctionArgs::Exprs(args),
            filter,
            over: None,
            ..
        } => {
            for arg in args {
                resolve_expr(arg, columns);
            }
            if let Some(filter) = filter {
                resolve_expr(filter, columns);
            }
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            if let Some(operand) = operand {
                resolve_expr(operand, columns);
            }
            for (when, then) in branches {
                resolve_expr(when, columns);
                resolve_expr(then, columns);
            }
            if let Some(otherwise) = else_expr {
                resolve_expr(otherwise, columns);
            }
        }
        _ => {}
    }
}
