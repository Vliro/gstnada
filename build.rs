fn main() {
    gst_plugin_version_helper::info();

    println!("cargo:rustc-link-search=razor/lib");
    println!("cargo:rustc-flags=-l cc -L razor/lib")
   // println!("cargo:rustc-link-lib=static=cc");
}
