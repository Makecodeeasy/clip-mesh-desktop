// Tauri 构建脚本 — 由 tauri-build crate 在编译阶段执行。
// 主要职责：生成平台特定的资源嵌入（图标、配置）与 Windows manifest。
fn main() {
    tauri_build::build()
}
