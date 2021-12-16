// SPDX-FileCopyrightText: © 2021 ChiselStrike <info@chiselstrike.com>

use crate::db::{sql, Relation};
use crate::query::{DbConnection, Kind, QueryError};
use crate::types::{Field, ObjectDelta, ObjectType, Type};
use futures::stream::BoxStream;
use futures::stream::Stream;
use futures::StreamExt;
use itertools::zip;
use sea_query::{Alias, ColumnDef, Table};
use serde_json::json;
use sqlx::any::{Any, AnyPool, AnyRow};
use sqlx::Column;
use sqlx::Transaction;
use sqlx::{Executor, Row};
use std::cell::RefCell;
use std::marker::PhantomPinned;
use std::pin::Pin;
use std::task::{Context, Poll};

// Results directly out of the database
pub(crate) type RawSqlStream = BoxStream<'static, anyhow::Result<AnyRow>>;

// Results with policies applied
pub(crate) type JsonObject = serde_json::Map<String, serde_json::Value>;
pub(crate) type SqlStream = BoxStream<'static, anyhow::Result<JsonObject>>;

struct QueryResults {
    raw_query: String,
    // The streams we use in here only depend on the lifetime of raw_query.
    stream: RefCell<Option<RawSqlStream>>,
    _marker: PhantomPinned, // QueryStream cannot be moved
}

pub(crate) fn new_query_results(raw_query: String, pool: &AnyPool) -> RawSqlStream {
    let ret = Box::pin(QueryResults {
        raw_query,
        stream: RefCell::new(None),
        _marker: PhantomPinned,
    });
    let ptr: *const String = &ret.raw_query;
    let query = sqlx::query::<Any>(unsafe { &*ptr });
    let stream = query.fetch(pool).map(|i| i.map_err(anyhow::Error::new));
    ret.stream.replace(Some(Box::pin(stream)));
    ret
}

impl Stream for QueryResults {
    type Item = anyhow::Result<AnyRow>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut borrow = self.stream.borrow_mut();
        borrow.as_mut().unwrap().as_mut().poll_next(cx)
    }
}

impl TryFrom<&Field> for ColumnDef {
    type Error = anyhow::Error;
    fn try_from(field: &Field) -> anyhow::Result<Self> {
        let mut column_def = ColumnDef::new(Alias::new(&field.name));
        match field.type_ {
            Type::String => column_def.text(),
            Type::Int => column_def.integer(),
            Type::Id => column_def.text().unique_key().primary_key(),
            Type::Float => column_def.double(),
            Type::Boolean => column_def.boolean(),
            Type::Object(_) => {
                anyhow::bail!(QueryError::NotImplemented(
                    "support for type Object".to_owned(),
                ));
            }
        };

        Ok(column_def)
    }
}

/// Query engine.
///
/// The query engine provides a way to persist objects and retrieve them from
/// a backing store for ChiselStrike endpoints.
#[derive(Clone)]
pub(crate) struct QueryEngine {
    kind: Kind,
    pool: AnyPool,
}

impl QueryEngine {
    fn new(kind: Kind, pool: AnyPool) -> Self {
        Self { kind, pool }
    }

    pub(crate) async fn local_connection(conn: &DbConnection) -> anyhow::Result<Self> {
        let local = conn.local_connection().await?;
        Ok(Self::new(local.kind, local.pool))
    }

    pub(crate) async fn drop_table(
        &self,
        transaction: &mut Transaction<'_, Any>,
        ty: &ObjectType,
    ) -> anyhow::Result<()> {
        let drop_table = Table::drop()
            .table(Alias::new(ty.backing_table()))
            .to_owned();
        let drop_table = drop_table.build_any(DbConnection::get_query_builder(&self.kind));
        let drop_table = sqlx::query(&drop_table);

        transaction
            .execute(drop_table)
            .await
            .map_err(QueryError::ExecuteFailed)?;
        Ok(())
    }

    pub(crate) async fn start_transaction(&self) -> anyhow::Result<Transaction<'_, Any>> {
        Ok(self
            .pool
            .begin()
            .await
            .map_err(QueryError::ConnectionFailed)?)
    }

    pub(crate) async fn commit_transaction(
        transaction: Transaction<'_, Any>,
    ) -> anyhow::Result<()> {
        transaction
            .commit()
            .await
            .map_err(QueryError::ExecuteFailed)?;
        Ok(())
    }

    pub(crate) async fn create_table(
        &self,
        transaction: &mut Transaction<'_, Any>,
        ty: &ObjectType,
    ) -> anyhow::Result<()> {
        let mut create_table = Table::create()
            .table(Alias::new(ty.backing_table()))
            .if_not_exists()
            .to_owned();

        for field in ty.all_fields() {
            let mut column_def = ColumnDef::try_from(field)?;
            create_table.col(&mut column_def);
        }
        let create_table = create_table.build_any(DbConnection::get_query_builder(&self.kind));

        let create_table = sqlx::query(&create_table);
        transaction
            .execute(create_table)
            .await
            .map_err(QueryError::ExecuteFailed)?;
        Ok(())
    }

    pub(crate) async fn alter_table(
        &self,
        transaction: &mut Transaction<'_, Any>,
        old_ty: &ObjectType,
        delta: ObjectDelta,
    ) -> anyhow::Result<()> {
        let mut table = Table::alter()
            .table(Alias::new(old_ty.backing_table()))
            .to_owned();

        let mut needs_alter = false;
        for field in delta.added_fields.iter() {
            needs_alter = true;
            let mut column_def = ColumnDef::try_from(field)?;
            table.add_column(&mut column_def);
        }

        for field in delta.removed_fields.iter() {
            needs_alter = true;
            table.drop_column(Alias::new(&field.name));
        }
        // We don't loop over the modified part of the delta: SQLite doesn't support modify columns
        // at all, but that is fine since the currently supported field modifications are handled
        // by ChiselStrike directly and require no modifications to the tables.
        //
        // There are modifications that we can accept on application side (like changing defaults),
        // since we always write with defaults. For all others, we should error out way before we
        // get here.

        if !needs_alter {
            return Ok(());
        }

        // alter table is problematic on SQLite (https://sqlite.org/lang_altertable.html)
        //
        // However there are some modifications that are safe (like adding a column or removing a
        // non-foreign-key column), but sqlx doesn't even generate the statement for them.
        //
        // So we fake being Postgres. Our ALTERs should be well-behaved, but we then need to make
        // sure we're not doing any kind of operation that are listed among the problematic ones.
        //
        // In particular, we can't use defaults, which is fine since we can handle that on
        // chiselstrike's side.
        //
        // FIXME: When we start generating indexes or using foreign keys, we'll have to make sure
        // that those are still safe. Adding columns is always safe, but removals may not be if
        // they are used in relations or indexes (see the document above)
        let table = table.build_any(DbConnection::get_query_builder(&Kind::Postgres));

        let table = sqlx::query(&table);
        transaction
            .execute(table)
            .await
            .map_err(QueryError::ExecuteFailed)?;
        Ok(())
    }

    pub(crate) fn query_relation(&self, rel: Relation) -> SqlStream {
        sql(&self.pool, rel)
    }

    pub(crate) async fn add_row(
        &self,
        ty: &ObjectType,
        ty_value: &serde_json::Value,
    ) -> anyhow::Result<()> {

        let mut field_binds = String::new();
        let mut field_names = String::new();

        for (i, f) in ty.all_fields().enumerate() {
            field_binds.push_str(&std::format!("${},", i + 1));
            field_names.push_str(&f.name);
            field_names.push(",");
        }
        field_binds.pop();
        field_names.pop();

        let insert_query = std::format!(
            "INSERT INTO {} ({}) VALUES ({})",
            &ty.backing_table(),
            field_names,
            field_binds
        );

        let mut insert_query = sqlx::query(&insert_query);
        for field in ty.all_fields() {
            macro_rules! bind_default_json_value {
                (str, $value:expr) => {{
                    insert_query = insert_query.bind($value);
                }};
                ($fallback:ident, $value:expr) => {{
                    let value: $fallback = $value.as_str().parse().map_err(|_| {
                        QueryError::IncompatibleData(field.name.to_owned(), $value.clone())
                    })?;
                    insert_query = insert_query.bind(value);
                }};
            }

            macro_rules! bind_json_value {
                ($as_type:ident, $fallback:ident ) => {{
                    match ty_value.get(&field.name) {
                        Some(value_json) => {
                            let value = value_json.$as_type().ok_or_else(|| {
                                QueryError::IncompatibleData(
                                    field.name.to_owned(),
                                    ty.name().to_owned(),
                                )
                            })?;
                            insert_query = insert_query.bind(value);
                        }
                        None => {
                            let value = field.generate_value().ok_or_else(|| {
                                QueryError::IncompatibleData(
                                    field.name.to_owned(),
                                    ty.name().to_owned(),
                                )
                            })?;
                            bind_default_json_value!($fallback, value);
                        }
                    }
                }};
            }

            match field.type_ {
                Type::String => bind_json_value!(as_str, str),
                Type::Int => bind_json_value!(as_i64, i64),
                Type::Id => bind_json_value!(as_str, str),
                Type::Float => bind_json_value!(as_f64, f64),
                Type::Boolean => bind_json_value!(as_bool, bool),
                Type::Object(_) => {
                    anyhow::bail!(QueryError::NotImplemented(
                        "support for type Object".to_owned(),
                    ));
                }
            }
        }

        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(QueryError::ConnectionFailed)?;
        transaction
            .execute(insert_query)
            .await
            .map_err(QueryError::ExecuteFailed)?;
        transaction
            .commit()
            .await
            .map_err(QueryError::ExecuteFailed)?;
        Ok(())
    }
}

pub(crate) fn relational_row_to_json(
    columns: &[(String, Type)],
    row: &AnyRow,
) -> anyhow::Result<JsonObject> {
    let mut ret = JsonObject::default();
    for (query_column, result_column) in zip(columns, row.columns()) {
        let i = result_column.ordinal();
        // FIXME: consider result_column.type_info().is_null() too
        macro_rules! to_json {
            ($value_type:ty) => {{
                let val = row.get::<$value_type, _>(i);
                json!(val)
            }};
        }
        let val = match query_column.1 {
            Type::Float => to_json!(f64),
            Type::Int => to_json!(i64),
            Type::String => to_json!(&str),
            Type::Id => to_json!(&str),
            Type::Boolean => to_json!(bool),
            Type::Object(_) => unreachable!("A column cannot be a Type::Object"),
        };
        ret.insert(result_column.name().to_string(), val);
    }
    Ok(ret)
}
