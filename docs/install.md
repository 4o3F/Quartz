# 安装与使用说明

Quartz 是解压即用的单个程序，不需要安装程序，也不联网。

## 安装

1. 从 GitHub Release 下载 `quartz-<版本>-x86_64-pc-windows-msvc.zip`，可用同名 `.zip.sha256` 或压缩包内的 `SHA256SUMS` 核对完整性。
2. 解压到一个固定位置，例如 `%LOCALAPPDATA%\Programs\Quartz\`。之后不要再移动它；移动后需在设置里重新打开开机自启。
3. 双击 `quartz.exe`。Quartz 在托盘区出现图标并开始记录，点击图标打开界面。
4. 在 **设置 → 常规** 中打开 **开机自启**。它写入 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Quartz`，只影响当前用户，不需要管理员权限。

## 浏览器扩展

1. 运行 `deno task package-extension`，生成 `target/extension/chromium` 与 `target/extension/firefox`。
2. Chrome / Edge：打开扩展管理页，开启 **开发者模式**，点击 **加载已解压的扩展程序** 并选择 `target/extension/chromium`。
3. Firefox：打开 `about:debugging`，选择 **临时加载** 并载入 `target/extension/firefox/manifest.json`。
4. 扩展通过同目录的 `quartz-host.exe` 连接 Quartz；宿主注册由 **设置 → 浏览器** 完成（下一批次）。

## 数据

- 数据目录为 `%LOCALAPPDATA%\Quartz\`：`quartz.db` 为数据库，`logs\` 为日志，`backups\` 为备份。程序目录与数据目录分开，替换程序不会影响数据。
- **设置 → 数据** 可查看数据库位置与大小、立即备份、压缩数据库，以及按日期范围删除已结束的日子。
- **设置 → 记录** 可设置标题保留期；到期的窗口与网页标题会被清除，时长与应用记录保留。
- 删除数据和清除标题都不会修改已有备份；如需彻底清除，请同时删除 `backups\` 中对应的文件。

## 更新

1. 在托盘菜单中选择 **退出 Quartz**。
2. 用新版本的 `quartz.exe`（和 `quartz-host.exe`）覆盖旧文件。
3. 重新启动 `quartz.exe`。如果数据库需要升级，Quartz 会先在 `backups\` 中写一份 `pre-migrate-*.db` 备份再升级。

## 恢复备份

退出 Quartz 后，在命令行中运行：

```
quartz.exe restore-backup "%LOCALAPPDATA%\Quartz\backups\<备份文件>.db" --yes
```

不带 `--yes` 时只显示将要发生的操作并退出。恢复会用备份替换当前数据库，备份之后记录的数据会丢失。

## 卸载

1. 在 **设置 → 常规** 关闭开机自启。
2. 从托盘退出 Quartz，删除程序目录。
3. 如不再需要数据，删除 `%LOCALAPPDATA%\Quartz\`。
