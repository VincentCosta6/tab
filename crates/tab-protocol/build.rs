fn main() {
    let protos = ["proto/tab/hub/v1/hub.proto", "proto/tab/cloud/v1/account.proto"];
    for p in protos {
        println!("cargo:rerun-if-changed={p}");
    }
    let fds = protox::compile(protos, ["proto"]).expect("the .proto files should compile");
    prost_build::Config::new()
        .compile_fds(fds)
        .expect("prost should generate Rust from the .proto files");
}
