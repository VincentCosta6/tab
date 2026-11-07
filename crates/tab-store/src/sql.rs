//! The hub's tables and queries, built with sea-query so the same
//! statements render for SQLite (desktop, Durable Object) and Postgres
//! (an org's own database, later).
//!
//! Backends render these with their dialect's builder and run them on their
//! own driver: rusqlite on the desktop, `storage.sql` inside a Durable
//! Object, sqlx for Postgres. That's why this isn't an ORM: no Rust ORM can
//! drive the Durable Object's SQLite, which is only reachable from inside
//! the Worker, but a query builder works everywhere.
//!
//! Full-text search is the one thing that differs per database (FTS5
//! trigrams in SQLite, `pg_trgm` in Postgres), so each backend writes that
//! part itself.

use sea_query::{
    ColumnDef, Expr, ExprTrait, Iden, Index, IndexCreateStatement, OnConflict, Order, Query,
    SelectStatement, Table, TableCreateStatement,
};

#[derive(Iden, Clone, Copy)]
pub enum HubLog {
    Table,
    Seq,
    Op,
    ByWhom,
    At,
}

#[derive(Iden, Clone, Copy)]
pub enum HubRecords {
    Table,
    Rid,
    Kind,
    Id,
    Body,
    Seq,
    UpdatedAt,
}

#[derive(Iden, Clone, Copy)]
pub enum HubEdges {
    Table,
    FromNode,
    ToNode,
    Rel,
}

#[derive(Iden, Clone, Copy)]
pub enum HubLeases {
    Table,
    Key,
    Holder,
    Token,
    ExpiresAt,
}

#[derive(Iden, Clone, Copy)]
pub enum HubLeaseTokens {
    Table,
    Key,
    Token,
}

/// The portable tables. Timestamps are RFC 3339 text with nanoseconds,
/// fixed width, so comparing the text compares the times.
pub fn tables() -> Vec<TableCreateStatement> {
    vec![
        Table::create()
            .table(HubLog::Table)
            .if_not_exists()
            .col(
                ColumnDef::new(HubLog::Seq)
                    .big_integer()
                    .not_null()
                    .primary_key(),
            )
            .col(ColumnDef::new(HubLog::Op).text().not_null())
            .col(ColumnDef::new(HubLog::ByWhom).text().not_null())
            .col(ColumnDef::new(HubLog::At).text().not_null())
            .to_owned(),
        Table::create()
            .table(HubRecords::Table)
            .if_not_exists()
            .col(
                ColumnDef::new(HubRecords::Rid)
                    .big_integer()
                    .not_null()
                    .auto_increment()
                    .primary_key(),
            )
            .col(ColumnDef::new(HubRecords::Kind).text().not_null())
            .col(ColumnDef::new(HubRecords::Id).text().not_null())
            .col(ColumnDef::new(HubRecords::Body).text().not_null())
            .col(ColumnDef::new(HubRecords::Seq).big_integer().not_null())
            .col(ColumnDef::new(HubRecords::UpdatedAt).text().not_null())
            .index(
                Index::create()
                    .unique()
                    .col(HubRecords::Kind)
                    .col(HubRecords::Id),
            )
            .to_owned(),
        Table::create()
            .table(HubEdges::Table)
            .if_not_exists()
            .col(ColumnDef::new(HubEdges::FromNode).text().not_null())
            .col(ColumnDef::new(HubEdges::ToNode).text().not_null())
            .col(ColumnDef::new(HubEdges::Rel).text().not_null())
            .primary_key(
                Index::create()
                    .col(HubEdges::FromNode)
                    .col(HubEdges::ToNode)
                    .col(HubEdges::Rel),
            )
            .to_owned(),
        Table::create()
            .table(HubLeases::Table)
            .if_not_exists()
            .col(
                ColumnDef::new(HubLeases::Key)
                    .text()
                    .not_null()
                    .primary_key(),
            )
            .col(ColumnDef::new(HubLeases::Holder).text().not_null())
            .col(ColumnDef::new(HubLeases::Token).big_integer().not_null())
            .col(ColumnDef::new(HubLeases::ExpiresAt).text().not_null())
            .to_owned(),
        Table::create()
            .table(HubLeaseTokens::Table)
            .if_not_exists()
            .col(
                ColumnDef::new(HubLeaseTokens::Key)
                    .text()
                    .not_null()
                    .primary_key(),
            )
            .col(
                ColumnDef::new(HubLeaseTokens::Token)
                    .big_integer()
                    .not_null(),
            )
            .to_owned(),
    ]
}

pub fn indexes() -> Vec<IndexCreateStatement> {
    vec![
        Index::create()
            .if_not_exists()
            .name("hub_edges_to")
            .table(HubEdges::Table)
            .col(HubEdges::ToNode)
            .to_owned(),
    ]
}

pub fn head() -> SelectStatement {
    Query::select()
        .expr(Expr::col(HubLog::Seq).max())
        .from(HubLog::Table)
        .to_owned()
}

pub fn insert_log(seq: i64, op: String, by: &str, at: &str) -> sea_query::InsertStatement {
    Query::insert()
        .into_table(HubLog::Table)
        .columns([HubLog::Seq, HubLog::Op, HubLog::ByWhom, HubLog::At])
        .values_panic([seq.into(), op.into(), by.into(), at.into()])
        .to_owned()
}

pub fn changes_since(since: i64, limit: u64) -> SelectStatement {
    Query::select()
        .columns([HubLog::Seq, HubLog::Op, HubLog::ByWhom, HubLog::At])
        .from(HubLog::Table)
        .and_where(Expr::col(HubLog::Seq).gt(since))
        .order_by(HubLog::Seq, Order::Asc)
        .limit(limit)
        .to_owned()
}

/// Upserts a record, returning its row id.
pub fn upsert_record(
    kind: &str,
    id: &str,
    body: String,
    seq: i64,
    at: &str,
) -> sea_query::InsertStatement {
    Query::insert()
        .into_table(HubRecords::Table)
        .columns([
            HubRecords::Kind,
            HubRecords::Id,
            HubRecords::Body,
            HubRecords::Seq,
            HubRecords::UpdatedAt,
        ])
        .values_panic([kind.into(), id.into(), body.into(), seq.into(), at.into()])
        .on_conflict(
            OnConflict::columns([HubRecords::Kind, HubRecords::Id])
                .update_columns([HubRecords::Body, HubRecords::Seq, HubRecords::UpdatedAt])
                .to_owned(),
        )
        .returning_col(HubRecords::Rid)
        .to_owned()
}

/// Deletes a record, returning its row id if it existed.
pub fn delete_record(kind: &str, id: &str) -> sea_query::DeleteStatement {
    Query::delete()
        .from_table(HubRecords::Table)
        .and_where(Expr::col(HubRecords::Kind).eq(kind))
        .and_where(Expr::col(HubRecords::Id).eq(id))
        .returning_col(HubRecords::Rid)
        .to_owned()
}

pub fn get_record(kind: &str, id: &str) -> SelectStatement {
    Query::select()
        .columns([
            HubRecords::Kind,
            HubRecords::Id,
            HubRecords::Body,
            HubRecords::Seq,
            HubRecords::UpdatedAt,
        ])
        .from(HubRecords::Table)
        .and_where(Expr::col(HubRecords::Kind).eq(kind))
        .and_where(Expr::col(HubRecords::Id).eq(id))
        .to_owned()
}

pub fn delete_edges_of(node: &str) -> sea_query::DeleteStatement {
    Query::delete()
        .from_table(HubEdges::Table)
        .cond_where(
            Expr::col(HubEdges::FromNode)
                .eq(node)
                .or(Expr::col(HubEdges::ToNode).eq(node)),
        )
        .to_owned()
}

pub fn insert_edge(from: &str, to: &str, rel: &str) -> sea_query::InsertStatement {
    Query::insert()
        .into_table(HubEdges::Table)
        .columns([HubEdges::FromNode, HubEdges::ToNode, HubEdges::Rel])
        .values_panic([from.into(), to.into(), rel.into()])
        .on_conflict(
            OnConflict::columns([HubEdges::FromNode, HubEdges::ToNode, HubEdges::Rel])
                .do_nothing()
                .to_owned(),
        )
        .to_owned()
}

pub fn delete_edge(from: &str, to: &str, rel: &str) -> sea_query::DeleteStatement {
    Query::delete()
        .from_table(HubEdges::Table)
        .and_where(Expr::col(HubEdges::FromNode).eq(from))
        .and_where(Expr::col(HubEdges::ToNode).eq(to))
        .and_where(Expr::col(HubEdges::Rel).eq(rel))
        .to_owned()
}

/// Edges touching `node`, sorted by (from, to, rel) in byte order.
pub fn neighbors(node: &str, limit: u64) -> SelectStatement {
    Query::select()
        .columns([HubEdges::FromNode, HubEdges::ToNode, HubEdges::Rel])
        .from(HubEdges::Table)
        .cond_where(
            Expr::col(HubEdges::FromNode)
                .eq(node)
                .or(Expr::col(HubEdges::ToNode).eq(node)),
        )
        .order_by(HubEdges::FromNode, Order::Asc)
        .order_by(HubEdges::ToNode, Order::Asc)
        .order_by(HubEdges::Rel, Order::Asc)
        .limit(limit)
        .to_owned()
}

const LEASE_COLS: [HubLeases; 4] = [
    HubLeases::Key,
    HubLeases::Holder,
    HubLeases::Token,
    HubLeases::ExpiresAt,
];

pub fn get_lease(key: &str) -> SelectStatement {
    Query::select()
        .columns(LEASE_COLS)
        .from(HubLeases::Table)
        .and_where(Expr::col(HubLeases::Key).eq(key))
        .to_owned()
}

pub fn last_token(key: &str) -> SelectStatement {
    Query::select()
        .column(HubLeaseTokens::Token)
        .from(HubLeaseTokens::Table)
        .and_where(Expr::col(HubLeaseTokens::Key).eq(key))
        .to_owned()
}

pub fn put_lease(
    key: &str,
    holder: &str,
    token: i64,
    expires_at: &str,
) -> sea_query::InsertStatement {
    Query::insert()
        .into_table(HubLeases::Table)
        .columns(LEASE_COLS)
        .values_panic([key.into(), holder.into(), token.into(), expires_at.into()])
        .on_conflict(
            OnConflict::column(HubLeases::Key)
                .update_columns([HubLeases::Holder, HubLeases::Token, HubLeases::ExpiresAt])
                .to_owned(),
        )
        .to_owned()
}

pub fn put_token(key: &str, token: i64) -> sea_query::InsertStatement {
    Query::insert()
        .into_table(HubLeaseTokens::Table)
        .columns([HubLeaseTokens::Key, HubLeaseTokens::Token])
        .values_panic([key.into(), token.into()])
        .on_conflict(
            OnConflict::column(HubLeaseTokens::Key)
                .update_column(HubLeaseTokens::Token)
                .to_owned(),
        )
        .to_owned()
}

pub fn delete_lease(key: &str) -> sea_query::DeleteStatement {
    Query::delete()
        .from_table(HubLeases::Table)
        .and_where(Expr::col(HubLeases::Key).eq(key))
        .to_owned()
}

/// Deletes leases that lapsed by `now`, returning them.
pub fn expire_leases(now: &str) -> sea_query::DeleteStatement {
    Query::delete()
        .from_table(HubLeases::Table)
        .and_where(Expr::col(HubLeases::ExpiresAt).lte(now))
        .returning(Query::returning().columns(LEASE_COLS))
        .to_owned()
}

/// Bodies of records that may mention a file hash (the caller checks).
pub fn bodies_mentioning_blobs() -> SelectStatement {
    Query::select()
        .column(HubRecords::Body)
        .from(HubRecords::Table)
        .and_where(Expr::col(HubRecords::Body).like("%sha256:%"))
        .to_owned()
}

pub fn count_records() -> SelectStatement {
    Query::select()
        .expr(Expr::col(HubRecords::Rid).count())
        .from(HubRecords::Table)
        .to_owned()
}

pub fn count_edges() -> SelectStatement {
    Query::select()
        .expr(Expr::col(HubEdges::Rel).count())
        .from(HubEdges::Table)
        .to_owned()
}

pub fn count_live_leases(now: &str) -> SelectStatement {
    Query::select()
        .expr(Expr::col(HubLeases::Key).count())
        .from(HubLeases::Table)
        .and_where(Expr::col(HubLeases::ExpiresAt).gt(now))
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_query::{PostgresQueryBuilder, SqliteQueryBuilder};

    #[test]
    fn statements_render_for_sqlite_and_postgres() {
        for t in tables() {
            assert!(t.to_string(SqliteQueryBuilder).starts_with("CREATE TABLE"));
            assert!(
                t.to_string(PostgresQueryBuilder)
                    .starts_with("CREATE TABLE")
            );
        }
        let (pg, _) = upsert_record("item", "1", "{}".into(), 1, "t").build(PostgresQueryBuilder);
        assert!(
            pg.contains("ON CONFLICT") && pg.contains("RETURNING") && pg.contains("$1"),
            "{pg}"
        );
        let (lite, _) = expire_leases("t").build(SqliteQueryBuilder);
        assert!(lite.contains("RETURNING"), "{lite}");
    }
}
