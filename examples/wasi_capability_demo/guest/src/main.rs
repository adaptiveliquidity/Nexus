use std::fs;
use std::io::Write;
use std::process;

fn main() {
    // Phase 1: Read from the allowed input directory
    let csv = match fs::read_to_string("/input/orders.csv") {
        Ok(data) => data,
        Err(e) => {
            eprintln!("ERROR: failed to read /input/orders.csv: {e}");
            process::exit(1);
        }
    };

    // Phase 2: Parse CSV and compute a summary report
    let mut total_revenue = 0.0_f64;
    let mut line_count = 0_u32;
    let mut report_lines: Vec<String> = Vec::new();

    for (i, line) in csv.lines().enumerate() {
        if i == 0 {
            continue; // skip header
        }
        let cols: Vec<&str> = line.split(',').collect();
        if cols.len() < 4 {
            continue;
        }
        let product = cols[1];
        let qty: f64 = cols[2].parse().unwrap_or(0.0);
        let price: f64 = cols[3].parse().unwrap_or(0.0);
        let subtotal = qty * price;
        total_revenue += subtotal;
        line_count += 1;
        report_lines.push(format!("  {product}: {qty:.0} x ${price:.2} = ${subtotal:.2}"));
    }

    let report = format!(
        "=== Order Summary Report ===\nItems processed: {line_count}\n{}\nTotal revenue: ${total_revenue:.2}\n",
        report_lines.join("\n")
    );

    // Phase 3: Write report to the allowed output directory
    match fs::File::create("/output/report.txt") {
        Ok(mut f) => {
            if let Err(e) = f.write_all(report.as_bytes()) {
                eprintln!("ERROR: failed to write /output/report.txt: {e}");
                process::exit(2);
            }
        }
        Err(e) => {
            eprintln!("ERROR: failed to write /output/report.txt: {e}");
            process::exit(2);
        }
    }

    // Phase 4: Attempt unauthorized read from /secrets — must fail
    match fs::read_to_string("/secrets/fake-token.txt") {
        Ok(contents) => {
            eprintln!("SECURITY VIOLATION: read secret: {contents}");
            process::exit(99);
        }
        Err(_) => {
            // Expected: capability system blocked the read
        }
    }

    match fs::write("/input/mutated.csv", b"tamper") {
        Ok(_) => {
            eprintln!("SECURITY VIOLATION: wrote to read-only /input");
            process::exit(98);
        }
        Err(_) => {
            // Expected: /input is mounted read-only.
        }
    }

    process::exit(0);
}
