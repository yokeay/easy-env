# easyenv

一键初始化开发环境的桌面工具。基于 Tauri 2 构建。

## 功能

- 可视化管理开发环境（自定义创建，配置软件名称、安装目录、下载地址、环境变量）
- 拖拽排序安装顺序
- 下载到系统 Downloads 目录，安装到指定目标目录
- 自动检测 bin 目录并配置系统 PATH（Windows / macOS / Linux）
- 安装中支持终止并自动回滚（删除已安装文件）
- 实时进度展示（树形 timeline）
- 数据持久化（环境配置保存到本地，重启不丢失）
- 关闭窗口后台继续运行（系统托盘）
- 版本检查更新（对接 GitHub Releases）
- 11 种语言支持
- 白天/夜间主题切换

## 构建

```bash
git tag v0.1.3
git push origin v0.1.3
```

产物：Windows (.exe/.msi) | macOS (.dmg) | Linux (.deb/.AppImage)

## 本地开发

```bash
cargo install tauri-cli --version "^2"
cargo tauri dev
```

## 许可

MIT
