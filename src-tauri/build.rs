use std::path::{Path, PathBuf};

fn main() {
    // MinGW 的 windres 读不了非 ASCII 路径（本仓库路径含中文），
    // 所以把窗口图标先复制到 OUT_DIR（纯 ASCII），再把 tauri-build 指过去。
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR 未设置");
    let icon_src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("icons")
        .join("icon.ico");
    let icon_dst: PathBuf = Path::new(&out_dir).join("winres-icon.ico");

    if icon_src.exists() {
        // 成功走静默（cargo:warning 会把正常构建日志染黄）；失败必须让人看见。
        if let Err(e) = std::fs::copy(&icon_src, &icon_dst) {
            println!("cargo:warning=复制 icon.ico 失败（{e}），将使用默认图标");
        }
    }

    let windows = tauri_build::WindowsAttributes::new().window_icon_path(&icon_dst);

    tauri_build::try_build(tauri_build::Attributes::new().windows_attributes(windows))
        .expect("tauri-build 失败");

    // 阶段 11.6：cargo test 的测试 exe 没有应用清单（tauri-winres 只管正式 bin），
    // Windows 便加载 comctl32 5.82，缺 TaskDialogIndirect 这个入口点，
    // 测试进程一起动就 0xC0000139。这里给测试目标单独补一份声明
    // Common-Controls v6 的清单资源（用 windres 预编译好的 COFF 对象）。
    #[cfg(target_os = "windows")]
    {
        if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu")
            && std::env::var("CARGO_CFG_TARGET_POINTER_WIDTH").as_deref() == Ok("64")
        {
            let src = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("build-support")
                .join("test-manifest-x64-gnu.o");
            if let Ok(dst) =
                std::fs::copy(&src, Path::new(&out_dir).join("test-manifest-x64-gnu.o"))
            {
                if dst > 0 {
                    // 注意是 rustc-link-arg（不带 -lib / -bins）：`-lib` 只作用于库本体，
                    // **到不了 lib 单测 exe**（实测：测试二进制没有清单，一起动就 0xC0000139），
                    // 通用形式对 bins / tests 都生效。代价：正式 bin 会多链接一份内容相同的
                    // comctl32 清单，ld 报一条可忽略的 merge 警告，产物可用性已实测验证。
                    println!(
                        "cargo:rustc-link-arg={}",
                        Path::new(&out_dir)
                            .join("test-manifest-x64-gnu.o")
                            .display()
                    );
                }
            }
        }
    }

    println!("cargo:rerun-if-changed=icons/icon.ico");
    println!("cargo:rerun-if-changed=tauri.conf.json");
    println!("cargo:rerun-if-changed=capabilities");
    println!("cargo:rerun-if-changed=build-support/test-manifest-x64-gnu.o");
}
