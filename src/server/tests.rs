use super::*;

/// Regression test for a real panic this was added to fix:
/// `build_day_report` builds (and, at the end of the call, drops) a
/// `reqwest::blocking::Client` when JIRA is configured and a viewed
/// day has a JIRA-linked website visit. `reqwest::blocking` spins up
/// its own mini tokio runtime internally; doing that from a worker
/// thread already inside `axum::serve`'s runtime panics ("Cannot drop
/// a runtime in a context where blocking is not allowed") — every
/// `/api/report` request for a JIRA-linked day crashed its connection
/// before `spawn_blocking_result` existed. Reproduces the exact
/// mechanism directly (build + drop a blocking client) rather than
/// standing up a real server/DB/JIRA fixture, since the bug is about
/// which thread runs the closure, not anything JIRA- or DB-specific.
#[tokio::test]
async fn spawn_blocking_result_survives_dropping_a_reqwest_blocking_client() {
    let result: anyhow::Result<()> = spawn_blocking_result(|| {
        let _client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(1))
            .build()?;
        Ok(())
    })
    .await;
    assert!(result.is_ok(), "{result:?}");
}

/// The happy path: a successful blocking closure's value comes back
/// through unchanged.
#[tokio::test]
async fn spawn_blocking_result_returns_the_closures_value() {
    let result = spawn_blocking_result(|| Ok(2 + 2)).await;
    assert_eq!(result.unwrap(), 4);
}

/// An `Err` from the closure propagates as-is, not just a generic
/// "task panicked"/join error.
#[tokio::test]
async fn spawn_blocking_result_propagates_the_closures_error() {
    let result: anyhow::Result<()> =
        spawn_blocking_result(|| Err(anyhow::anyhow!("boom"))).await;
    assert_eq!(result.unwrap_err().to_string(), "boom");
}
