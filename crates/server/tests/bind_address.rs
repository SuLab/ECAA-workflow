use ecaa_workflow_server::resolve_bind_addr;

// These tests mutate the process-global `ECAA_BIND_ADDR` env var.
// `#[serial_test::serial]` (keyed on the var name) prevents them from
// racing each other under `cargo test` and from racing any other
// env-touching test in this crate.

#[serial_test::serial(ECAA_BIND_ADDR)]
#[test]
fn default_bind_is_localhost() {
    std::env::remove_var("ECAA_BIND_ADDR");
    let addr = resolve_bind_addr(3000);
    assert_eq!(addr, "127.0.0.1:3000");
}

#[serial_test::serial(ECAA_BIND_ADDR)]
#[test]
fn explicit_wildcard_bind_requires_env() {
    std::env::set_var("ECAA_BIND_ADDR", "0.0.0.0");
    let addr = resolve_bind_addr(3000);
    assert_eq!(addr, "0.0.0.0:3000");
    std::env::remove_var("ECAA_BIND_ADDR");
}

#[serial_test::serial(ECAA_BIND_ADDR)]
#[test]
fn explicit_bind_address_passes_through() {
    std::env::set_var("ECAA_BIND_ADDR", "192.168.1.4");
    let addr = resolve_bind_addr(3000);
    assert_eq!(addr, "192.168.1.4:3000");
    std::env::remove_var("ECAA_BIND_ADDR");
}
