use super::{ProcessStat, delta, memory, process_stat, same_process_delta, storage_io};

#[test]
fn process_name_can_contain_a_closing_parenthesis() {
    let raw = "8 (a) b) S 0 0 0 0 0 0 0 0 0 0 20 3 0 0 0 0 0 0 91 82 12";
    assert_eq!(
        process_stat(raw),
        Some(ProcessStat {
            name: "a) b",
            ticks: 23,
            started: 91,
            rss_pages: Some(12)
        })
    );
}

#[test]
fn malformed_process_stat_is_unknown() {
    assert_eq!(process_stat("8 (a) R 1 2"), None);
}

#[test]
fn missing_available_memory_is_unknown() {
    assert_eq!(memory("MemTotal: 1024 kB\nMemFree: 32 kB\n"), None);
}

#[test]
fn memory_uses_available_instead_of_free() {
    assert_eq!(
        memory("MemTotal: 1024 kB\nMemFree: 32 kB\nMemAvailable: 768 kB\n"),
        Some((256, 1024))
    );
}

#[test]
fn logical_and_physical_io_are_distinct() {
    let raw = "rchar: 500\nwchar: 900\nread_bytes: 12\nwrite_bytes: 9\ncancelled_write_bytes: 5\n";
    assert_eq!(storage_io(raw), Some((12, 9)));
}

#[test]
fn missing_storage_counters_are_unknown() {
    assert_eq!(storage_io("rchar: 500\nwchar: 900\n"), None);
}

#[test]
fn reset_counter_cannot_produce_a_rate() {
    assert_eq!(delta(2, 9), None);
}

#[test]
fn reused_pid_cannot_inherit_the_old_rate() {
    assert_eq!(same_process_delta(45, 44, 200, 100), None);
}

#[test]
fn matching_process_uses_counter_delta() {
    assert_eq!(same_process_delta(44, 44, 200, 100), Some(100));
}

#[test]
fn cpu_is_computed_from_total_minus_idle_and_io_wait() {
    let old = [10, 0, 10, 70, 10, 0, 0, 0];
    let new = [20, 0, 30, 130, 20, 0, 0, 0];
    assert_eq!(super::cpu_tenths(new, old), Some(300));
}

#[test]
fn disk_rate_uses_fixed_512_byte_sectors_and_elapsed_time() {
    assert_eq!(
        super::per_second(10, 512, std::time::Duration::from_secs(2)),
        Some(2560)
    );
}

#[test]
fn wireless_level_accepts_the_kernel_updated_marker() {
    let sample = "wlan0: 0000 66. -44. -256 0 0 0 0 0 0\n";
    assert_eq!(super::wireless_signal(sample, "wlan0"), Some(-44));
}
