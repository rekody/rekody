fn main() {
    #[cfg(target_os = "macos")]
    {
        // CGEventTap lives in ApplicationServices.
        println!("cargo:rustc-link-lib=framework=ApplicationServices");
        // CFDictionary / CFString / CFRelease / kCFBooleanTrue live in CoreFoundation.
        println!("cargo:rustc-link-lib=framework=CoreFoundation");
    }
}
