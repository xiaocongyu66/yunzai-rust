use std::path::PathBuf;
use std::env;

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let dest = out.join("yz_bridge.node");
    // CI：YZ_BRIDGE_BIN 指向同架构编译好的 libyz_bridge.(so|dylib)
    if let Ok(src) = env::var("YZ_BRIDGE_BIN") {
        if !src.is_empty() {
            let src = PathBuf::from(&src);
            if src.is_file() {
                std::fs::copy(&src, &dest).expect("拷贝 yz_bridge 到 OUT_DIR 失败");
                println!("cargo:rerun-if-env-changed=YZ_BRIDGE_BIN");
                println!("cargo:rerun-if-changed={}", src.display());
                return;
            }
            panic!("YZ_BRIDGE_BIN 指向的文件不存在: {}", src.display());
        }
    }
    // 本地开发兜底：空占位（运行时回退发行包 lib/ 文件）
    std::fs::write(&dest, b"").expect("写 yz_bridge 占位失败");
}
