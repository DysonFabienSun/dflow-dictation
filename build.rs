fn main() {
    println!("cargo:rerun-if-changed=assets/dflow.rc");
    println!("cargo:rerun-if-changed=assets/dflow.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile_for("assets/dflow.rc", ["dflow"], embed_resource::NONE)
            .manifest_required()
            .expect("could not embed the DFlow executable icon");
    }
}
