fn main() {
    gst_plugin_version_helper::info();

    cc::Build::new().files(
        [
            "nada/sender-based-controller.cc",
            "nada/nada-controller.cc",
            "nada/nada_glue.cpp"
        ]
    )
        .include("nada")
        .compile("nada");
}
