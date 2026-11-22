use super::AllowedHosts;

#[test]
fn dns_aliases_are_trimmed_and_case_insensitive() -> anyhow::Result<()> {
    let hosts = AllowedHosts::parse(" Monitor.Example.Test., display ")?;
    assert!(hosts.contains("monitor.example.test") && hosts.contains("DISPLAY."));
    Ok(())
}

#[test]
fn aliases_validate_and_bound_input() {
    for bad in [
        "localhost",
        "LOCALHOST.",
        "192.0.2.30",
        "2001:db8::1",
        "*.example.test",
        "dashboard.example.test:8080",
        "a..example.test",
        "-a.example.test",
        "a_.example.test",
        "a.example.test/",
        "a.example.test,",
    ] {
        assert!(AllowedHosts::parse(bad).is_err(), "{bad}");
    }
    let too_many = ["host"; 17].join(",");
    assert!(AllowedHosts::parse(&too_many).is_err());
}
