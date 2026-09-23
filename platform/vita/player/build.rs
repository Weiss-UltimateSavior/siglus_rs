fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("vita") {
        return;
    }
    // Link published VitaSDK packages; no third-party source is copied into
    // this workspace. Static archive order follows vitaGL's dependencies.
    for library in [
        "vitaGL",
        "vitashark",
        "SceShaccCgExt",
        "taihen_stub",
        "SceShaccCg_stub",
        "mathneon",
        "SceGxm_stub",
        "SceKernelDmacMgr_stub",
        "SceSysmodule_stub",
        "SceAppMgr_stub",
        "SceDisplay_stub",
        "SceCommonDialog_stub",
        "SceSysmem_stub",
        "stdc++",
        "m",
    ] {
        println!("cargo:rustc-link-lib=static={library}");
    }
}
