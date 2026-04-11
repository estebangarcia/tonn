fn main() {
    #[cfg(windows)]
    {
        winresource::WindowsResource::new()
            .set_icon("../../assets/tonn.ico")
            .compile()
            .expect("failed to compile Windows resources");
    }
}
