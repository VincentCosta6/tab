fn main() {
    let proto = "proto/tab/hub/v1/hub.proto";
    println!("cargo:rerun-if-changed={proto}");
    let fds = protox::compile([proto], ["proto"]).expect("hub.proto should compile");
    prost_build::Config::new()
        .compile_fds(fds)
        .expect("prost should generate Rust from hub.proto");
}
