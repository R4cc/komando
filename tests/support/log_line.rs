/// Log line `i`, about 96 bytes: a dim timestamp, a coloured level, a service and a message.
pub fn log_line(i: u64, out: &mut Vec<u8>) {
    use std::io::Write;
    let (color, level) = match i % 16 {
        0 => ("31", "ERROR"),
        1 | 2 => ("33", "WARN "),
        3 => ("35", "DEBUG"),
        _ => ("32", "INFO "),
    };
    let services = ["api", "worker", "auth", "billing", "scheduler"];
    let service = services[(i % 5) as usize];
    let _ = write!(
        out,
        "\x1b[2m2026-11-13 {:02}:{:02}:{:02}.{:03}\x1b[0m \x1b[{color}m{level}\x1b[0m \x1b[36m{service:<9}\x1b[0m GET /api/items/{} {} in {}ms\r\n",
        (i / 3_600_000) % 24,
        (i / 60_000) % 60,
        (i / 1000) % 60,
        i % 1000,
        i % 100_000,
        if i.is_multiple_of(16) { 500 } else { 200 },
        i % 997,
    );
}

/// Log lines `0..lines` in blocks of about 64 KiB, which a server sends one at a time as the
/// client's window has room. Made as they are taken, so `lines` can be endless in practice.
pub fn log_blocks(lines: u64) -> impl Iterator<Item = Vec<u8>> {
    let mut next = 0;
    std::iter::from_fn(move || {
        if next == lines {
            return None;
        }
        let mut block = Vec::with_capacity(70 * 1024);
        while next < lines && block.len() < 64 * 1024 {
            log_line(next, &mut block);
            next += 1;
        }
        Some(block)
    })
}
