fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("vita") {
        return;
    }
    // Link published VitaSDK packages; no third-party source is copied into
    // this workspace. vita2d carries its shaders precompiled, so no runtime
    // shader compiler (libshacccg.suprx) is needed on the device.
    for library in [
        "vita2d",
        "SceGxm_stub",
        "SceDisplay_stub",
        "SceSysmodule_stub",
        "SceCommonDialog_stub",
        "SceAppUtil_stub",
        "SceAppMgr_stub",
        "SceSysmem_stub",
        "m",
    ] {
        println!("cargo:rustc-link-lib=static={library}");
    }
}
