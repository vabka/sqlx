use super::{Postgres, PostgresQueryParameters, PostgresRawConnection, PostgresRow};
use crate::{
    connection::RawConnection,
    describe::{Describe, ResultField},
    postgres::raw::Step,
    query::QueryParameters,
    url::Url,
};
use async_trait::async_trait;
use futures_core::stream::BoxStream;

use std::sync::atomic::{AtomicU64, Ordering};

use crate::postgres::{protocol::Message, PostgresDatabaseError};
use std::hash::Hasher;

#[async_trait]
impl RawConnection for PostgresRawConnection {
    type Backend = Postgres;

    async fn establish(url: &str) -> crate::Result<Self> {
        let url = Url::parse(url)?;
        let address = url.resolve(5432);
        let mut conn = Self::new(address).await?;

        conn.startup(
            url.username(),
            url.password().unwrap_or_default(),
            url.database(),
        )
        .await?;

        Ok(conn)
    }

    #[inline]
    async fn close(mut self) -> crate::Result<()> {
        self.terminate().await
    }

    async fn execute(
        &mut self,
        query: &str,
        params: PostgresQueryParameters,
    ) -> crate::Result<u64> {
        self.parse("", query, &params);
        self.bind("", "", &params);
        self.execute("", 1);
        self.sync().await?;

        let mut affected = 0;

        while let Some(step) = self.step().await? {
            if let Step::Command(cnt) = step {
                affected = cnt;
            }
        }

        Ok(affected)
    }

    fn fetch(
        &mut self,
        query: &str,
        params: PostgresQueryParameters,
    ) -> BoxStream<'_, crate::Result<PostgresRow>> {
        self.parse("", query, &params);
        self.bind("", "", &params);
        self.execute("", 0);

        Box::pin(async_stream::try_stream! {
            self.sync().await?;

            while let Some(step) = self.step().await? {
                if let Step::Row(row) = step {
                    yield row;
                }
            }
        })
    }

    async fn fetch_optional(
        &mut self,
        query: &str,
        params: PostgresQueryParameters,
    ) -> crate::Result<Option<PostgresRow>> {
        self.parse("", query, &params);
        self.bind("", "", &params);
        self.execute("", 2);
        self.sync().await?;

        let mut row: Option<PostgresRow> = None;

        while let Some(step) = self.step().await? {
            if let Step::Row(r) = step {
                if row.is_some() {
                    return Err(crate::Error::FoundMoreThanOne);
                }

                row = Some(r);
            }
        }

        Ok(row)
    }

    async fn describe(&mut self, body: &str) -> crate::Result<Describe<Postgres>> {
        self.parse("", body, &PostgresQueryParameters::new());
        self.describe("");
        self.sync().await?;

        let param_desc = loop {
            let step = self
                .step()
                .await?
                .ok_or(invalid_data!("did not receive ParameterDescription"));

            if let Step::ParamDesc(desc) = step? {
                break desc;
            }
        };

        let row_desc = loop {
            let step = self
                .step()
                .await?
                .ok_or(invalid_data!("did not receive RowDescription"));

            if let Step::RowDesc(desc) = step? {
                break desc;
            }
        };

        Ok(Describe {
            param_types: param_desc.ids.into_vec(),
            result_fields: row_desc
                .fields
                .into_vec()
                .into_iter()
                .map(|field| ResultField {
                    name: Some(field.name),
                    table_id: Some(field.table_id),
                    type_id: field.type_id,
                })
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::QueryParameters;
    use std::env;

    fn database_url() -> String {
        env::var("POSTGRES_DATABASE_URL")
            .or_else(|_| env::var("DATABASE_URL"))
            .unwrap()
    }

    #[tokio::test]
    #[ignore]
    async fn it_establishes() -> crate::Result<()> {
        let mut conn = PostgresRawConnection::establish(&database_url()).await?;

        // After establish, run PING to ensure that it was established correctly
        conn.ping().await?;

        // Then explicitly close the connection
        conn.close().await?;

        Ok(())
    }

    #[tokio::test]
    #[ignore]
    async fn it_executes() -> crate::Result<()> {
        let mut conn = PostgresRawConnection::establish(&database_url()).await?;

        let affected_rows_from_begin =
            RawConnection::execute(&mut conn, "BEGIN", PostgresQueryParameters::new()).await?;

        assert_eq!(affected_rows_from_begin, 0);

        let affected_rows_from_create_table = RawConnection::execute(
            &mut conn,
            r#"
CREATE TEMP TABLE sqlx_test_it_executes (
    id BIGSERIAL PRIMARY KEY
)
                "#,
            PostgresQueryParameters::new(),
        )
        .await?;

        assert_eq!(affected_rows_from_create_table, 0);

        for _ in 0..5_i32 {
            let affected_rows_from_insert = RawConnection::execute(
                &mut conn,
                "INSERT INTO sqlx_test_it_executes DEFAULT VALUES",
                PostgresQueryParameters::new(),
            )
            .await?;

            assert_eq!(affected_rows_from_insert, 1);
        }

        let affected_rows_from_delete = RawConnection::execute(
            &mut conn,
            "DELETE FROM sqlx_test_it_executes",
            PostgresQueryParameters::new(),
        )
        .await?;

        assert_eq!(affected_rows_from_delete, 5);

        let affected_rows_from_rollback =
            RawConnection::execute(&mut conn, "ROLLBACK", PostgresQueryParameters::new()).await?;

        assert_eq!(affected_rows_from_rollback, 0);

        Ok(())
    }
}