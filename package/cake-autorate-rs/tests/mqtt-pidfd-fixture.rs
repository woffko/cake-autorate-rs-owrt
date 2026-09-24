//! No network, filesystem writes, or signal handling. The test owns the stdin
//! writer and Child handle for this process; exit/cleanup is always parent-owned.
fn main() {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let _ = std::io::copy(&mut input, &mut std::io::sink());
}
