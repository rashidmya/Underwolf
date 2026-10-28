fn main() {
    if std::env::var_os("CARGO_FEATURE_NODE").is_some()
        && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux")
    {
        napi_build::setup();
    }
}
