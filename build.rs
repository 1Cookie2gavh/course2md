fn main() {
    // shared 模式：dylib 与二进制同目录（crate build script 已复制），补上 @loader_path
    println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path");
}
