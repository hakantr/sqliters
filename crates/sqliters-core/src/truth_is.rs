//! Resolve the legacy truth comparison in SQL scopes with known source columns.

use sqliters_syntax::{
    BinaryOp, ColumnRef, CommonTableExpr, Expr, FromClause, FunctionArgs, QualifiedName,
    ResultColumn, SelectCore, Stmt, TableOrSubquery, UnaryOp,
};

use crate::db_set::DatabaseSet;
use crate::schema::TableSchema;

#[derive(Clone, Copy, Default)]
struct TruthScope<'a> {
    table: Option<&'a TableSchema>,
    true_column: Option<&'a str>,
    false_column: Option<&'a str>,
    where_aliases: [Option<&'a Expr>; 2],
}

impl<'a> TruthScope<'a> {
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
            let Some(mut scope) = select_truth_scope(from.as_ref(), &select.ctes, db_set) else {
                return;
            };
            for column in projection.iter_mut() {
                if let ResultColumn::Expr { expr, .. } = column {
                    resolve_expr(expr, scope);
                }
            }
            let aliases = ["false", "true"].map(|name| {
                projection
                    .iter()
                    .enumerate()
                    .find_map(|(index, column)| match column {
                        ResultColumn::Expr {
                            expr,
                            alias: Some(alias),
                            ..
                        } if alias.eq_ignore_ascii_case(name) => Some((index, expr)),
                        _ => None,
                    })
            });
            // WHERE looks for a source column first, then copies the first matching
            // result expression. ORDER BY has a separate alias-first rule.
            scope.where_aliases = [false, true].map(|value| {
                aliases[usize::from(value)]
                    .filter(|_| scope.for_keyword(value).is_none())
                    .map(|(_, expr)| expr)
            });
            if let Some(predicate) = selection
                && (scope.where_aliases.iter().all(Option::is_none)
                    || (select.ctes.is_empty()
                        && scope.table.is_some_and(|table| {
                            scope
                                .where_aliases
                                .iter()
                                .flatten()
                                .all(|expr| scalar_where_alias(expr, table))
                        })))
            {
                resolve_expr(predicate, scope);
            }
            if aliases.iter().all(Option::is_none) {
                for term in &mut select.order_by {
                    if select.ctes.is_empty()
                        && let Some(table) = scope.table
                    {
                        resolve_unary_order_truth_name(&mut term.expr, scope, table);
                    }
                    resolve_expr(&mut term.expr, scope);
                }
            } else if select.ctes.is_empty()
                && let Some(table) = scope.table
            {
                use sqliters_codegen::consts::{SQLITE_FullColNames, SQLITE_ShortColNames};

                // Both flags must differ from their defaults to qualify expanded names.
                let column_name_flags = SQLITE_FullColNames | SQLITE_ShortColNames;
                let long_names = db_set.c_flags_toggled() & column_name_flags == column_name_flags;
                for term in &mut select.order_by {
                    if let Expr::Bool(value) = term.expr
                        && let Some((index, _)) = aliases[usize::from(value)]
                    {
                        // C marks each star-expanded column as a result name. A matching
                        // short name from an earlier star can precede the explicit alias.
                        let star_match = scope
                            .for_keyword(value)
                            .filter(|_| !long_names)
                            .and_then(|name| table.find_column(name).map(|(index, _)| index));
                        let mut ordinal = 1;
                        for column in &projection[..index] {
                            match column {
                                ResultColumn::Expr { .. } => ordinal += 1,
                                ResultColumn::Star | ResultColumn::TableStar { .. } => {
                                    if let Some(star_index) = star_match {
                                        ordinal += star_index;
                                        break;
                                    }
                                    ordinal += table.columns.len();
                                }
                            }
                        }
                        term.expr = Expr::Integer(ordinal as i64);
                    } else {
                        resolve_unary_order_truth_name(&mut term.expr, scope, table);
                    }
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

fn scalar_where_alias(expr: &Expr, table: &TableSchema) -> bool {
    match expr {
        Expr::Column(column) => {
            column.schema.is_none()
                && column.table.is_none()
                && (table.find_column(&column.column).is_some()
                    || table.is_implicit_rowid_name(&column.column))
        }
        Expr::Null
        | Expr::Bool(_)
        | Expr::Integer(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Blob(_) => true,
        _ => false,
    }
}

/// Resolve a wrapped ORDER BY name with source-column precedence.
fn resolve_unary_order_truth_name(
    order_expr: &mut Expr,
    scope: TruthScope<'_>,
    table: &TableSchema,
) {
    let mut operand = &mut *order_expr;
    let mut unary_plus = false;
    let mut explicit_collation = None;
    loop {
        match operand {
            Expr::Collate { expr, collation } => {
                explicit_collation.get_or_insert_with(|| collation.clone());
                operand = expr;
            }
            Expr::Unary {
                op: UnaryOp::Plus,
                expr,
            } => {
                unary_plus = true;
                operand = expr;
            }
            _ => break,
        }
    }
    if !unary_plus {
        return;
    }
    let Expr::Bool(value) = operand else { return };
    let resolved = if let Some(name) = scope.for_keyword(*value) {
        Expr::Column(ColumnRef::unqualified(name))
    } else if let Some(alias) = scope.where_aliases[usize::from(*value)] {
        let supported = match alias {
            Expr::Unary {
                op: UnaryOp::Negate,
                expr,
            }
            | Expr::Collate { expr, .. } => scalar_where_alias(expr, table),
            Expr::Binary {
                op: BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply,
                left,
                right,
            } => scalar_where_alias(left, table) && scalar_where_alias(right, table),
            _ => scalar_where_alias(alias, table),
        };
        if !supported {
            return;
        }
        if let Expr::Collate { collation, .. } = alias {
            explicit_collation.get_or_insert_with(|| collation.clone());
        }
        alias.clone()
    } else {
        return;
    };
    let collation = explicit_collation.or_else(|| {
        let Expr::Column(column) = &resolved else {
            return None;
        };
        let (_, definition) = table.find_column(&column.column)?;
        let name = definition.collation_name();
        (!name.eq_ignore_ascii_case("BINARY")).then(|| name.to_owned())
    });
    // The plus wrapper keeps the substituted integer on the expression path.
    *operand = resolved;
    // The compiled sorter reads the effective collation from the ORDER BY root.
    if let Some(collation) = collation
        && !matches!(order_expr, Expr::Collate { .. })
    {
        *order_expr = Expr::Collate {
            expr: Box::new(std::mem::replace(order_expr, Expr::Null)),
            collation,
        };
    }
}

fn select_truth_scope<'a>(
    from: Option<&FromClause>,
    ctes: &[CommonTableExpr],
    db_set: &'a DatabaseSet,
) -> Option<TruthScope<'a>> {
    let Some(from) = from else {
        return Some(TruthScope::default());
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
            .then(TruthScope::default);
    }
    table_truth_columns(name, db_set)
}

fn table_truth_columns<'a>(
    name: &QualifiedName,
    db_set: &'a DatabaseSet,
) -> Option<TruthScope<'a>> {
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
        return Some(TruthScope {
            table: Some(table),
            true_column: table
                .find_column("true")
                .map(|(_, column)| column.name.as_str()),
            false_column: table
                .find_column("false")
                .map(|(_, column)| column.name.as_str()),
            ..TruthScope::default()
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
        .map(|_| TruthScope::default())
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

fn resolve_expr(expr: &mut Expr, scope: TruthScope<'_>) {
    match expr {
        Expr::Bool(value) => {
            if let Some(name) = scope.for_keyword(*value) {
                *expr = Expr::Column(ColumnRef::unqualified(name));
            } else if let Some(alias) = scope.where_aliases[usize::from(*value)] {
                *expr = alias.clone();
            }
        }
        Expr::Binary { op, left, right } => {
            resolve_expr(left, scope);
            resolve_expr(right, scope);
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
        | Expr::IsNull { expr, .. } => resolve_expr(expr, scope),
        Expr::FunctionCall {
            args: FunctionArgs::Exprs(args),
            filter,
            over: None,
            ..
        } => {
            for arg in args {
                resolve_expr(arg, scope);
            }
            if let Some(filter) = filter {
                resolve_expr(filter, scope);
            }
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            if let Some(operand) = operand {
                resolve_expr(operand, scope);
            }
            for (when, then) in branches {
                resolve_expr(when, scope);
                resolve_expr(then, scope);
            }
            if let Some(otherwise) = else_expr {
                resolve_expr(otherwise, scope);
            }
        }
        _ => {}
    }
}
