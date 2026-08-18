fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!(
        "cargo:rustc-env=CSA_BUILD_TARGET={}",
        std::env::var("TARGET").expect("Cargo always sets TARGET for build scripts")
    );
}
