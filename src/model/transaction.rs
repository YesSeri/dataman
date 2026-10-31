//! Operation boundaries shared by database mutations and imports.
use crate::error::{AppError, AppResult};
use rusqlite::Connection;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

pub(crate) fn atomic<T>(
    connection: &Connection,
    cancellation: Option<&Arc<AtomicBool>>,
    operation: impl FnOnce() -> AppResult<T>,
) -> AppResult<T> {
    let cancelled = || cancellation.is_some_and(|token| token.load(Ordering::Relaxed));
    if cancelled() {
        return Err(AppError::Cancelled);
    }
    if !connection.is_autocommit() {
        return operation();
    }
    // This guard runs before SQLite's transaction guard, including on unwind.
    struct CancellationGuard<'a> {
        connection: &'a Connection,
        token: Option<&'a Arc<AtomicBool>>,
    }
    impl Drop for CancellationGuard<'_> {
        fn drop(&mut self) {
            if let Some(token) = self.token {
                super::database::Database::install_cancellation(self.connection, token.clone());
            }
        }
    }
    let _restore = CancellationGuard {
        connection,
        token: cancellation,
    };
    let mut transaction = connection.unchecked_transaction()?;
    struct CleanupGuard<'a>(&'a Connection, bool);
    impl Drop for CleanupGuard<'_> {
        fn drop(&mut self) {
            if self.1 {
                self.0.progress_handler(0, None::<fn() -> bool>);
            }
        }
    }
    let cleanup = CleanupGuard(connection, cancellation.is_some());
    let result = operation().and_then(|value| {
        if cancelled() {
            Err(AppError::Cancelled)
        } else {
            Ok(value)
        }
    });
    drop(cleanup);
    let result = result.and_then(|value| {
        connection.execute_batch("COMMIT")?;
        Ok(value)
    });
    let result = match result {
        Err(error) => crate::error::finish(
            Err(error.normalize_cancelled(cancelled())),
            if connection.is_autocommit() {
                Ok(())
            } else {
                connection.execute_batch("ROLLBACK").map_err(AppError::from)
            },
        ),
        success => success,
    };
    transaction.set_drop_behavior(rusqlite::DropBehavior::Ignore);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_helper_does_not_commit_outer_transaction() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE t(x); BEGIN")
            .unwrap();
        atomic(&connection, None, || {
            connection.execute_batch("INSERT INTO t VALUES(1)")?;
            Ok(())
        })
        .unwrap();
        assert!(!connection.is_autocommit());
        connection.execute_batch("ROLLBACK").unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM t", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn panic_rolls_back_even_after_cancellation() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE t(x)").unwrap();
        let token = Arc::new(AtomicBool::new(false));
        super::super::database::Database::install_cancellation(&connection, token.clone());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: AppResult<()> = atomic(&connection, Some(&token), || {
                connection.execute_batch("INSERT INTO t VALUES(1)")?;
                token.store(true, Ordering::Relaxed);
                panic!("operation panic");
            });
        }));
        assert!(result.is_err());
        assert!(connection.is_autocommit());
        token.store(false, Ordering::Relaxed);
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM t", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}
