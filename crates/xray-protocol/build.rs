fn main() -> Result<(), Box<dyn std::error::Error>> {
    std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);
    tonic_build::configure()
        .build_client(true)
        .build_server(false)
        .compile_protos(
            &[
                "../../proto/xray/common/serial/typed_message.proto",
                "../../proto/xray/common/protocol/user.proto",
                "../../proto/xray/proxy/vless/account.proto",
                "../../proto/xray/app/proxyman/command/command.proto",
                "../../proto/xray/app/stats/command/command.proto",
            ],
            &["../../proto/xray"],
        )?;
    println!("cargo:rerun-if-changed=../../proto/xray");
    Ok(())
}
