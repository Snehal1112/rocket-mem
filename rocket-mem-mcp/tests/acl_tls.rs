mod support;

use redis::AsyncCommands;
use rocket_mem_mcp::pool::Pool;

#[tokio::test]
async fn connecting_without_credentials_to_an_acl_instance_fails() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("acl-no-creds-test.aof");
    let (mut guard, addr) = support::spawn_rocket_mem_with_acl(&aof_path, "app", "changeme");

    let pool = Pool::connect(&addr, None, None, None).await;
    let result = match pool {
        // The pool itself may connect fine (auth happens per-command in some redis-rs setups);
        // either way, a command against an ACL-enabled instance with no credentials must fail.
        Ok(pool) => {
            let mut conn = pool.connection();
            conn.get::<_, Option<String>>("anything").await.map(|_| ())
        }
        Err(err) => Err(err),
    };
    assert!(
        result.is_err(),
        "a command with no ACL credentials against an ACL-enabled instance should fail"
    );

    guard.kill();
}

#[tokio::test]
async fn connecting_with_the_right_acl_credentials_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("acl-right-creds-test.aof");
    let (mut guard, addr) = support::spawn_rocket_mem_with_acl(&aof_path, "app", "changeme");

    let pool = Pool::connect(&addr, Some("app"), Some("changeme"), None)
        .await
        .expect("pool should connect and authenticate");
    let mut conn = pool.connection();
    let _: () = conn.set("acl-test-key", "acl-test-value").await.unwrap();
    let value: String = conn.get("acl-test-key").await.unwrap();
    assert_eq!(value, "acl-test-value");

    guard.kill();
}

#[tokio::test]
async fn connecting_over_tls_with_the_ca_cert_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("tls-test.aof");
    let (mut guard, _plain_addr, tls_addr) = support::spawn_rocket_mem_with_tls(&aof_path);

    let ca_path = support::tls_fixture_cert_path();
    let pool = Pool::connect(&tls_addr, None, None, Some(&ca_path))
        .await
        .expect("pool should connect over TLS");
    let mut conn = pool.connection();
    let _: () = conn.set("tls-test-key", "tls-test-value").await.unwrap();
    let value: String = conn.get("tls-test-key").await.unwrap();
    assert_eq!(value, "tls-test-value");

    guard.kill();
}

#[tokio::test]
async fn connecting_over_tls_with_acl_credentials_together_succeeds() {
    // Mirrors a real deployment shape: ACLs and TLS both enabled at once.
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("acl-and-tls-test.aof");
    let (mut guard, tls_addr) =
        support::spawn_rocket_mem_with_acl_and_tls(&aof_path, "app", "changeme");

    let ca_path = support::tls_fixture_cert_path();
    let pool = Pool::connect(&tls_addr, Some("app"), Some("changeme"), Some(&ca_path))
        .await
        .expect("pool should connect over TLS and authenticate");
    let mut conn = pool.connection();
    let _: () = conn
        .set("acl-and-tls-test-key", "acl-and-tls-test-value")
        .await
        .unwrap();
    let value: String = conn.get("acl-and-tls-test-key").await.unwrap();
    assert_eq!(value, "acl-and-tls-test-value");

    guard.kill();
}

#[tokio::test]
async fn connecting_without_tls_ca_path_to_a_plaintext_listener_still_works() {
    // Regression guard: adding TLS support must not break the plain, no-auth, no-TLS path
    // every other test in this crate already relies on.
    let dir = tempfile::tempdir().unwrap();
    let aof_path = dir.path().join("plain-after-tls-support-test.aof");
    let (mut guard, addr) = support::spawn_rocket_mem(&aof_path);

    let pool = Pool::connect(&addr, None, None, None)
        .await
        .expect("plain connection should still work");
    let mut conn = pool.connection();
    let _: () = conn.set("plain-key", "plain-value").await.unwrap();
    let value: String = conn.get("plain-key").await.unwrap();
    assert_eq!(value, "plain-value");

    guard.kill();
}
