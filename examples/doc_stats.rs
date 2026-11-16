//! Temporary: what a group document holds, and how long reading it takes.
fn main() {
    let bytes = std::fs::read(std::env::args().nth(1).unwrap()).unwrap();
    let doc = flipflop::groupdoc::GroupDoc::load(&bytes).unwrap();
    let t = std::time::Instant::now();
    let music = doc.music();
    let read = t.elapsed();
    let t = std::time::Instant::now();
    let files = doc.files();
    let files_read = t.elapsed();
    let stopped = music.iter().filter(|f| f.stopped).count();
    let tracks: usize = music.iter().map(|f| f.tracks.len()).sum();
    let hosts: usize = music
        .iter()
        .map(|f| f.hosts.len() + f.downloaded.len())
        .sum();
    println!(
        "music offers {} (stopped {stopped}), tracks {tracks}, hosts+downloaded {hosts}, read in {read:?}",
        music.len()
    );
    println!("files offers {}, read in {files_read:?}", files.len());
    for f in music.iter().take(5) {
        println!("  {} {} {}", f.kind, f.stopped, f.name);
    }
}
