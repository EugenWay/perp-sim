use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let idl_path = manifest_dir.join("src/vara/vara_perps.idl");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let out_path = out_dir.join("vara_perps_client.rs");

    println!("cargo:rerun-if-changed={}", idl_path.display());

    sails_client_gen::ClientGenerator::from_idl_path(&idl_path)
        .with_sails_crate("sails_rs")
        .with_client_path(&out_path)
        .generate()
        .expect("Failed to generate client from IDL");
}
