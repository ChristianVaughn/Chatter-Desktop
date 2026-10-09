//! Prints "down"/"up" for one key or mouse button, system-wide.
//!
//! Usage: cargo run -p chatter-hotkeys --example watch -- <DOM code> [seconds]
//! e.g.   cargo run -p chatter-hotkeys --example watch -- Backquote 30
//!        cargo run -p chatter-hotkeys --example watch -- Mouse4

use std::time::Duration;

use chatter_hotkeys::{Binding, HotkeyWatcher};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let code = args.next().unwrap_or_else(|| "Backquote".to_string());
    let seconds: u64 = match args.next() {
        Some(s) => s.parse()?,
        None => 10,
    };

    let binding = Binding::new(code);
    let watcher =
        HotkeyWatcher::start(|pressed| println!("{}", if pressed { "down" } else { "up" }))?;
    watcher.set_binding(Some(binding.clone()));
    println!(
        "watching {} ({}) via {} for {seconds}s",
        binding.code,
        binding.label(),
        watcher.backend().as_str()
    );
    std::thread::sleep(Duration::from_secs(seconds));
    drop(watcher);
    println!("stopped");
    Ok(())
}
