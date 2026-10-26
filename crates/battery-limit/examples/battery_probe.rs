//! Read-only probe: battery status, the detected SMC scheme, and which
//! charging-related SMC keys exist (and their current values).
//! Never writes anything.
//!
//!     cargo run -p battery-limit --example battery_probe

fn main() {
    let b = battery_limit::read_battery();
    println!("battery: {}", serde_json::to_string_pretty(&b).unwrap());
    println!("helper installed: {}", battery_limit::helper_installed());

    #[cfg(target_os = "macos")]
    {
        use battery_limit::smc::{decode_number, hex, AppleSmc, SmcBackend};
        match battery_limit::detect_scheme() {
            Ok(s) => println!("scheme: {} ({s:?})", s.name()),
            Err(e) => println!("scheme: error: {e:#}"),
        }
        let mut smc = match AppleSmc::open() {
            Ok(s) => s,
            Err(e) => return println!("AppleSMC: {e:#}"),
        };
        if std::env::args().any(|a| a == "--all") {
            let n = smc.key_count().unwrap_or(0);
            let keys: Vec<String> = (0..n).filter_map(|i| smc.key_at_index(i).ok()).collect();
            println!("{} SMC keys; charging-related (CH*, AC*, B*):", keys.len());
            for k in keys
                .iter()
                .filter(|k| k.starts_with("CH") || k.starts_with("AC") || k.starts_with('B'))
            {
                let info = smc.key_info(k);
                let val = smc
                    .read_key(k)
                    .map(|b| hex(&b))
                    .unwrap_or_else(|e| format!("<{e}>"));
                match info {
                    Ok(Some(i)) => println!("  {k} {} {} {val}", i.data_type, i.size),
                    _ => println!("  {k} ? {val}"),
                }
            }
        }
        println!(
            "\n{:<5} {:<6} {:>4}  {:<10} value",
            "key", "type", "size", "raw"
        );
        for key in [
            "CHTE", "CH0B", "CH0C", "CHIE", "CH0I", "CH0J", "CH0K", "ACLC", "BCLM", "CHWA", "CHBI",
            "TB0T", "TB1T", "TB2T", "B0AC", "B0FC", "B0RM", "BUIC",
        ] {
            match smc.key_info(key) {
                Ok(None) => println!("{key:<5} (absent)"),
                Err(e) => println!("{key:<5} info error: {e:#}"),
                Ok(Some(info)) => match smc.read_key(key) {
                    Ok(bytes) => println!(
                        "{key:<5} {:<6} {:>4}  {:<10} {}",
                        info.data_type,
                        info.size,
                        hex(&bytes),
                        decode_number(&info, &bytes)
                            .map(|v| format!("{v}"))
                            .unwrap_or_default()
                    ),
                    Err(e) => println!(
                        "{key:<5} {:<6} {:>4}  read error: {e:#}",
                        info.data_type, info.size
                    ),
                },
            }
        }
    }
}
