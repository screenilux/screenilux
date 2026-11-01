//! Build a recording's text index from the command line and search it.
//!
//!   cargo run --release -p screenilux-player --example read_text -- <recording> [query]
//!
//! The same work the Play tab's "Make text searchable" does, with timings, for
//! checking against real recordings.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use screenilux_player::open::{open, save_text};
use screenilux_player::text::{INDEX_INTERVAL, Indexer};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(args.next().expect("a recording to read"));
    let query = args.next();

    let opened = open(&path).expect("should open");
    println!(
        "{}: {} track(s), {:.1}s",
        opened.title,
        opened.recording.tracks.len(),
        opened.recording.duration
    );
    let start = Instant::now();
    let ix = Indexer::start(opened.recording.clone(), INDEX_INTERVAL, Arc::new(|| {}));
    let index = loop {
        if let Some(r) = ix.finished() {
            break r.expect("indexing should succeed");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let words: usize = index.entries.iter().map(|e| e.page.words().count()).sum();
    println!(
        "indexed in {:.1}s: {} entries, {words} words",
        start.elapsed().as_secs_f64(),
        index.entries.len()
    );
    let saved = save_text(&opened.origin, &index).expect("should save");
    println!("saved to {}", saved.display());

    if let Some(q) = query {
        for hit in index.search(&q) {
            println!("  {:>6.2}s  {}  {}", hit.t, hit.screen, hit.snippet);
        }
    }
}
