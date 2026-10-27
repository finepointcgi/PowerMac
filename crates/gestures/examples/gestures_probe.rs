//! Read-only check that MultitouchSupport delivers touch frames:
//! `cargo run -p gestures --example gestures_probe -- [seconds]`. Prints the devices,
//! a line per second with the frame count and current finger count, and
//! every gesture the recognizer sees (nothing is bound, nothing runs).

fn main() {
    #[cfg(target_os = "macos")]
    {
        use gestures::{reader::Reader, Frame, Recognizer, Thresholds};
        use std::sync::{Arc, Mutex};
        let secs: u64 = std::env::args()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(5);
        match gestures::availability() {
            Ok(d) => println!("devices: {d:?}"),
            Err(e) => {
                println!("{e}");
                return;
            }
        }
        let rec = Arc::new(Mutex::new(
            std::collections::HashMap::<usize, Recognizer>::new(),
        ));
        let last = Arc::new(Mutex::new((
            0usize,
            0.0f64,
            Vec::<(i32, f32, f32, bool)>::new(),
        )));
        let (r2, l2) = (rec.clone(), last.clone());
        let reader = Reader::start(Box::new(move |dev, f: &Frame| {
            *l2.lock().unwrap() = (
                f.contacts.iter().filter(|c| c.touching).count(),
                f.time,
                f.contacts
                    .iter()
                    .map(|c| (c.id, c.x, c.y, c.touching))
                    .collect(),
            );
            let mut m = r2.lock().unwrap();
            if let Some(g) = m
                .entry(dev)
                .or_insert_with(|| Recognizer::new(Thresholds::default()))
                .frame(f)
            {
                println!("gesture: {g}");
            }
        }));
        let reader = match reader {
            Ok(r) => r,
            Err(e) => {
                println!("{e}");
                return;
            }
        };
        println!("started: {:?}", reader.devices());
        for _ in 0..secs {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let l = last.lock().unwrap().clone();
            println!(
                "frames so far: {}, fingers: {}, t={:.3}, contacts: {:?}",
                Reader::frames_seen(),
                l.0,
                l.1,
                l.2
            );
        }
        drop(reader);
        println!("stopped; frames total: {}", Reader::frames_seen());
    }
    #[cfg(not(target_os = "macos"))]
    println!("{:?}", gestures::availability());
}
