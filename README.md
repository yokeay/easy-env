# easyenv

一键初始化开发环境的桌面工具。基于 Tauri 2 构建。

## 功能

- 可视化管理多个开发环境（Java, Flutter, Python, Rust, Node.js, Go, Docker 等）
- 自定义创建环境：配置软件名称、安装目录、下载地址、环境变量
- 拖拽排序安装顺序
- 实时安装进度展示（树形 timeline）
- 安装中支持终止与回滚
- 关闭窗口后台继续运行（系统托盘）
- 11 种语言支持：中文、蒙古文、藏文、阿拉伯文、英文、韩文、日文、法文、俄文、意大利文、西班牙文
- 白天/夜间主题切换

## 构建

通过 GitHub Actions 自动打包，推送 tag 触发：

```bash
git tag v0.1.0
git push origin v0.1.0
```

产物：Windows (.exe/.msi) | macOS (.dmg) | Linux (.deb/.AppImage)

### 本地开发

```bash
cargo install tauri-cli --version "^2"
cargo tauri dev
```

## 许可

MIT
