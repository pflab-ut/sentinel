fn main() {
    let stub_path = "src/child_stub.s";
    cc::Build::new().file(stub_path).compile("stub");
    println!("cargo:rerun-if-changed={}", stub_path);
}
